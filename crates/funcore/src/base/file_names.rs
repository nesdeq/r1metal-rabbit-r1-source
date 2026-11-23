//! File names: plainness, stems, counts, extensions.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

/// True for a single path component.
pub fn is_plain(name: &str) -> bool {
    !matches!(name, "" | "." | "..") && !name.contains('/')
}

// True where a name ends in a dot and that extension, in any case; a leading dot starts no extension.
pub(crate) fn has_extension(name: &str, extension: &str) -> bool {
    match name.rsplit_once('.') {
        Some((stem, suffix)) => !stem.is_empty() && suffix.eq_ignore_ascii_case(extension),
        None => false,
    }
}

/// A file name without its extension.
pub fn stem(name: &str) -> &str {
    name.rfind('.').map_or(name, |dot| &name[..dot])
}

/// The stem, with `_count` behind it from 1 on.
pub fn numbered_stem(stem: &str, count: u32) -> String {
    if count == 0 { String::from(stem) } else { format!("{stem}_{count}") }
}

/// A file name of `numbered_stem`.
pub fn numbered_file(stem: &str, extension: &str, count: u32) -> String {
    format!("{}.{extension}", numbered_stem(stem, count))
}

/// The candidate names of a new file, in order.
pub fn numbered_files<'a>(stem: &'a str, extension: &'a str) -> impl Iterator<Item = String> + 'a {
    (0..).map(move |count| numbered_file(stem, extension, count))
}

/// Directory names in listing order.
pub fn to_sorted(mut names: Vec<String>) -> Vec<String> {
    names.sort();
    names
}

/// The names in use in a directory.
pub trait Names {
    type Error;
    // True if the name is in use.
    fn has(&mut self, name: &str) -> Result<bool, Self::Error>;
}

/// The first name of `name_of` for 0, 1, 2 and on that `names` does not hold.
pub fn first_free<N: Names>(name_of: impl Fn(u32) -> String, mut names: N) -> Result<String, N::Error> {
    for count in 0.. {
        let name = name_of(count);
        if !names.has(&name)? {
            return Ok(name);
        }
    }
    unreachable!("a directory holds fewer names than a u32 counts")
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Stub<F>(F);

    impl<E, F: FnMut(&str) -> Result<bool, E>> Names for Stub<F> {
        type Error = E;

        fn has(&mut self, name: &str) -> Result<bool, E> {
            (self.0)(name)
        }
    }

    #[test]
    fn plain_names() {
        assert!(is_plain("20261012_190328") && is_plain(".hidden") && is_plain("a..b"));
        for none in ["", ".", "..", "../kept", "20261011_083000/..", "/", "a/b", "a/"] {
            assert!(!is_plain(none), "{none:?}");
        }
    }

    #[test]
    fn extension_follows_last_dot() {
        assert!(has_extension("20261009_193512.jpg", "jpg") && has_extension("A.JPG", "jpg"));
        assert!(has_extension("a.b.mp4", "mp4") && has_extension("..jpg", "jpg"));
        for none in ["notes", ".jpg", "photo.jpg.new", "photo.jpeg", "jpg"] {
            assert!(!has_extension(none, "jpg"), "{none:?}");
        }
    }

    #[test]
    fn first_free_skips_names_in_use() {
        let names = ["20261009_193512.jpg", "20261009_193512_1.jpg"];
        let is_taken = |name: &str| Ok::<_, ()>(names.contains(&name));
        assert_eq!(
            first_free(|count| numbered_file("20261009_193512", "jpg", count), Stub(is_taken)),
            Ok("20261009_193512_2.jpg".into())
        );
        assert_eq!(
            first_free(|count| numbered_stem("20261009_200000", count), Stub(is_taken)),
            Ok("20261009_200000".into())
        );
        assert_eq!(first_free(|count| numbered_stem("x", count), Stub(|_: &str| Err("unreadable"))), Err("unreadable"));
    }

    #[test]
    fn numbered_names() {
        assert_eq!(
            [0, 1, 2].map(|count| numbered_file("20261009_193512", "jpg", count)),
            ["20261009_193512.jpg", "20261009_193512_1.jpg", "20261009_193512_2.jpg"]
        );
        assert_eq!(
            (numbered_stem("20261009_193512", 0), numbered_stem("20261009_193512", 3)),
            ("20261009_193512".into(), "20261009_193512_3".into())
        );
        assert_eq!(
            (stem("20261009_193512_1.jpg"), stem("a.b.m4a"), stem("plain")),
            ("20261009_193512_1", "a.b", "plain")
        );
    }
}
