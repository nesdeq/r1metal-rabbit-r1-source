//! A flattened device tree, read into nodes and properties that can be changed, and written back as dtc writes it.

use crate::{be32, be64, invalid};
use std::io;

const MAGIC: u32 = 0xd00d_feed;
// The header's ten fields of four bytes each.
const HEADER_BYTES: usize = 40;
// The version this reader takes and writes, and the oldest a reader of it must know.
const VERSION: u32 = 17;
const LAST_COMPATIBLE: u32 = 16;
const BEGIN_NODE: u32 = 1;
const END_NODE: u32 = 2;
const PROPERTY: u32 = 3;
const NOP: u32 = 4;
const END: u32 = 9;
// The names a node's own phandle goes by.
const PHANDLE_NAMES: [&str; 2] = ["phandle", "linux,phandle"];
// How deep under the root a node may lie, as the kernel's own reader of trees takes them (`FDT_MAX_DEPTH`).
const MOST_DEPTH: usize = 64;

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Property {
    pub name: String,
    pub value: Vec<u8>,
}

#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Node {
    pub name: String,
    pub properties: Vec<Property>,
    pub children: Vec<Node>,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Tree {
    boot_cpu: u32,
    // The memory the tree reserves, as (address, size).
    reserved: Vec<(u64, u64)>,
    // The names of properties as the tree came, kept so that a tree nobody changed is written back byte for byte.
    strings: Vec<u8>,
    pub root: Node,
}

/// Values in cells, the big endian words a property holds.
pub fn cells(values: &[u32]) -> Vec<u8> {
    values.iter().flat_map(|value| value.to_be_bytes()).collect()
}

/// The cells a property holds; none if its length is no multiple of a cell.
pub fn words(value: &[u8]) -> Option<Vec<u32>> {
    let (whole, rest) = value.as_chunks::<4>();
    rest.is_empty().then(|| whole.iter().copied().map(u32::from_be_bytes).collect())
}

fn text(bytes: &[u8], at: usize) -> io::Result<&str> {
    let rest = bytes.get(at..).ok_or_else(|| invalid("a name past the end of a device tree"))?;
    let end = rest.iter().position(|&byte| byte == 0).ok_or_else(|| invalid("a name without its end"))?;
    std::str::from_utf8(&rest[..end]).map_err(|_| invalid("a name that is no text"))
}

struct Reader<'a> {
    structure: &'a [u8],
    strings: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn token(&mut self) -> io::Result<u32> {
        loop {
            let token = be32(self.structure, self.at)?;
            self.at += 4;
            if token != NOP {
                return Ok(token);
            }
        }
    }

    // The node whose BEGIN_NODE token was just read, `depth` under the root, with everything inside it.
    fn node(&mut self, depth: usize) -> io::Result<Node> {
        if depth >= MOST_DEPTH {
            return Err(invalid("a device tree deeper than a kernel reads one"));
        }
        let name = text(self.structure, self.at)?.to_string();
        self.at = (self.at + name.len() + 1).next_multiple_of(4);
        let mut node = Node { name, ..Node::default() };
        loop {
            match self.token()? {
                PROPERTY => {
                    let (length, name_at) =
                        (be32(self.structure, self.at)? as usize, be32(self.structure, self.at + 4)?);
                    let start = self.at + 8;
                    let value =
                        self.structure.get(start..start + length).ok_or_else(|| invalid("a property past the end"))?;
                    let name = text(self.strings, name_at as usize)?.to_string();
                    node.properties.push(Property { name, value: value.to_vec() });
                    self.at = (start + length).next_multiple_of(4);
                }
                BEGIN_NODE => node.children.push(self.node(depth + 1)?),
                END_NODE => return Ok(node),
                _ => return Err(invalid("a device tree with a token no tree has")),
            }
        }
    }
}

// Where `name` stands in the strings: the first place a string ends with it, as dtc looks; else it is added at the end.
fn name_at(strings: &mut Vec<u8>, name: &str) -> u32 {
    let mut ended = name.as_bytes().to_vec();
    ended.push(0);
    let found = (0..strings.len()).find(|&at| strings[at..].starts_with(&ended));
    let at = found.unwrap_or_else(|| {
        let end = strings.len();
        strings.extend_from_slice(&ended);
        end
    });
    at as u32
}

fn pad(out: &mut Vec<u8>) {
    out.resize(out.len().next_multiple_of(4), 0);
}

fn write_node(node: &Node, out: &mut Vec<u8>, strings: &mut Vec<u8>) {
    out.extend(BEGIN_NODE.to_be_bytes());
    out.extend(node.name.as_bytes());
    out.push(0);
    pad(out);
    for property in &node.properties {
        out.extend(PROPERTY.to_be_bytes());
        out.extend((property.value.len() as u32).to_be_bytes());
        out.extend(name_at(strings, &property.name).to_be_bytes());
        out.extend(&property.value);
        pad(out);
    }
    for child in &node.children {
        write_node(child, out, strings);
    }
    out.extend(END_NODE.to_be_bytes());
}

impl Tree {
    pub fn parse(blob: &[u8]) -> io::Result<Tree> {
        if be32(blob, 0)? != MAGIC {
            return Err(invalid("no flattened device tree"));
        }
        let field = |index: usize| be32(blob, 4 * index).map(|value| value as usize);
        let (total, structure_at, strings_at, reserved_at) = (field(1)?, field(2)?, field(3)?, field(4)?);
        if field(5)? != VERSION as usize {
            return Err(invalid("a device tree of a version this reader does not know"));
        }
        let blob = blob.get(..total).ok_or_else(|| invalid("a device tree shorter than it says"))?;
        let (strings_bytes, structure_bytes) = (field(8)?, field(9)?);
        let strings =
            blob.get(strings_at..strings_at + strings_bytes).ok_or_else(|| invalid("strings past the end"))?;
        let structure = blob
            .get(structure_at..structure_at + structure_bytes)
            .ok_or_else(|| invalid("a structure past the end"))?;
        let mut reserved = Vec::new();
        let mut at = reserved_at;
        loop {
            let (address, size) = (be64(blob, at)?, be64(blob, at + 8)?);
            at += 16;
            if (address, size) == (0, 0) {
                break;
            }
            reserved.push((address, size));
        }
        let mut reader = Reader { structure, strings, at: 0 };
        if reader.token()? != BEGIN_NODE {
            return Err(invalid("a device tree that does not begin with its root"));
        }
        let root = reader.node(0)?;
        if reader.token()? != END {
            return Err(invalid("a device tree with more after its root"));
        }
        Ok(Tree { boot_cpu: field(7)? as u32, reserved, strings: strings.to_vec(), root })
    }

    /// The tree laid out as dtc lays it out: header, reserved memory, structure, strings.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut strings = self.strings.clone();
        let mut structure = Vec::new();
        write_node(&self.root, &mut structure, &mut strings);
        structure.extend(END.to_be_bytes());
        let reserved_at = HEADER_BYTES;
        let structure_at = reserved_at + 16 * (self.reserved.len() + 1);
        let strings_at = structure_at + structure.len();
        let total = strings_at + strings.len();
        let mut out = Vec::with_capacity(total);
        for value in [
            MAGIC,
            total as u32,
            structure_at as u32,
            strings_at as u32,
            reserved_at as u32,
            VERSION,
            LAST_COMPATIBLE,
            self.boot_cpu,
            strings.len() as u32,
            structure.len() as u32,
        ] {
            out.extend(value.to_be_bytes());
        }
        for (address, size) in self.reserved.iter().chain([&(0, 0)]) {
            out.extend(address.to_be_bytes());
            out.extend(size.to_be_bytes());
        }
        out.extend(structure);
        out.extend(strings);
        out
    }

    /// The node at a path of names from the root, `/` alone being the root.
    pub fn node(&self, path: &str) -> Option<&Node> {
        path.split('/').filter(|name| !name.is_empty()).try_fold(&self.root, |node, name| node.child(name))
    }

    pub fn node_mut(&mut self, path: &str) -> Option<&mut Node> {
        path.split('/').filter(|name| !name.is_empty()).try_fold(&mut self.root, |node, name| node.child_mut(name))
    }

    /// The highest phandle a node of the tree holds; zero in a tree without any.
    pub fn highest_phandle(&self) -> u32 {
        fn highest(node: &Node) -> u32 {
            let own =
                PHANDLE_NAMES.iter().filter_map(|name| node.property(name)?.try_into().ok().map(u32::from_be_bytes));
            own.chain(node.children.iter().map(highest)).max().unwrap_or(0)
        }
        highest(&self.root)
    }
}

impl Node {
    pub fn named(name: &str) -> Node {
        Node { name: name.to_string(), ..Node::default() }
    }

    pub fn child(&self, name: &str) -> Option<&Node> {
        self.children.iter().find(|child| child.name == name)
    }

    pub fn child_mut(&mut self, name: &str) -> Option<&mut Node> {
        self.children.iter_mut().find(|child| child.name == name)
    }

    pub fn property(&self, name: &str) -> Option<&[u8]> {
        self.properties.iter().find(|property| property.name == name).map(|property| property.value.as_slice())
    }

    /// Gives a property this value, where it stands if the node has it, else after the others.
    pub fn set(&mut self, name: &str, value: impl Into<Vec<u8>>) {
        let value = value.into();
        match self.properties.iter_mut().find(|property| property.name == name) {
            Some(property) => property.value = value,
            None => self.properties.push(Property { name: name.to_string(), value }),
        }
    }

    /// Takes a property out, and gives back its value if the node had it.
    pub fn remove(&mut self, name: &str) -> Option<Vec<u8>> {
        let at = self.properties.iter().position(|property| property.name == name)?;
        Some(self.properties.remove(at).value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Tree {
        let mut root = Node::named("");
        root.set("#address-cells", cells(&[2]));
        root.set("model", b"R1\0".to_vec());
        let mut cpus = Node::named("cpus");
        let mut cpu = Node::named("cpu@000");
        cpu.set("reg", cells(&[0]));
        cpu.set("phandle", cells(&[0x0e]));
        cpus.children.push(cpu);
        let mut timer = Node::named("timer");
        timer.set("interrupts", cells(&[1, 13, 8]));
        timer.set("always-on", Vec::new());
        root.children.extend([cpus, timer]);
        Tree { boot_cpu: 0, reserved: vec![(0x4000_0000, 0x1000)], strings: Vec::new(), root }
    }

    #[test]
    fn tree_round_trip() {
        let tree = sample();
        let bytes = tree.to_bytes();
        assert_eq!(be32(&bytes, 0).unwrap(), MAGIC);
        assert_eq!(be32(&bytes, 4).unwrap() as usize, bytes.len());
        let read = Tree::parse(&bytes).unwrap();
        assert_eq!(read.root, tree.root);
        assert_eq!(read.reserved, tree.reserved);
        assert_eq!(read.to_bytes(), bytes, "a tree read and written again keeps its bytes");
    }

    #[test]
    fn shared_name_suffixes() {
        let mut strings = Vec::new();
        assert_eq!(name_at(&mut strings, "linux,phandle"), 0);
        assert_eq!(name_at(&mut strings, "phandle"), 6, "the end of a name that is there");
        assert_eq!(name_at(&mut strings, "reg"), 14);
        assert_eq!(strings, b"linux,phandle\0reg\0");
    }

    #[test]
    fn node_paths_and_properties() {
        let mut tree = sample();
        assert!(tree.node("/").is_some() && tree.node("/cpus/cpu@000").is_some());
        assert!(tree.node("/cpus/cpu@001").is_none() && tree.node("/nowhere").is_none());
        let timer = tree.node_mut("/timer").unwrap();
        timer.set("interrupts", cells(&[1, 13, 4]));
        assert_eq!(timer.properties[0].name, "interrupts", "a property that is there keeps its place");
        timer.set("status", b"okay\0".to_vec());
        assert_eq!(timer.properties.last().unwrap().name, "status");
        assert_eq!(timer.remove("always-on"), Some(Vec::new()));
        assert_eq!(timer.remove("always-on"), None);
        let read = Tree::parse(&tree.to_bytes()).unwrap();
        assert_eq!(words(read.node("/timer").unwrap().property("interrupts").unwrap()), Some(vec![1, 13, 4]));
        assert!(read.node("/timer").unwrap().property("always-on").is_none());
    }

    #[test]
    fn highest_phandle() {
        let mut tree = sample();
        assert_eq!(tree.highest_phandle(), 0x0e);
        tree.node_mut("/timer").unwrap().set("linux,phandle", cells(&[0xf0]));
        assert_eq!(tree.highest_phandle(), 0xf0);
    }

    #[test]
    fn foreign_tree_refused() {
        assert!(Tree::parse(&[0u8; 64]).is_err());
        let mut bytes = sample().to_bytes();
        bytes[23] = 16;
        assert!(Tree::parse(&bytes).is_err(), "another version");
        let mut bytes = sample().to_bytes();
        bytes.truncate(bytes.len() - 8);
        assert!(Tree::parse(&bytes).is_err(), "a tree cut short");
        assert!(be32(&bytes, bytes.len() - 3).is_err() && be32(&bytes, usize::MAX).is_err(), "a field past the end");
        assert_eq!(words(&[0, 0, 0, 1, 0]), None, "a value that is not whole cells");
    }

    #[test]
    fn tree_depth_limit() {
        let nested = |depth: usize| {
            let mut node = Node::named("leaf");
            for _ in 0..depth {
                let mut outer = Node::named("node");
                outer.children.push(node);
                node = outer;
            }
            Tree { root: Node { name: String::new(), ..node }, ..sample() }.to_bytes()
        };
        assert!(Tree::parse(&nested(MOST_DEPTH - 1)).is_ok(), "the deepest node a kernel takes");
        assert!(Tree::parse(&nested(MOST_DEPTH)).is_err(), "one further down");
    }
}
