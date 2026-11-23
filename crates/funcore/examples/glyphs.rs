//! Writes what the screen draws of the characters Spleen lacks: GNU Unifont's drawings and Noto Emoji's pictures.

use r1_funcore::gfx::emoji::{CODE_BYTES, COLOURS, KEY_CODES, PICTURE, SIDES, TONES};
use r1_funcore::gfx::{font, unifont};
use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fs;
use std::path::Path;

const MARGIN: usize = 1;
const OPAQUE: f32 = 0.5;
const FLAG_LETTER: u32 = 0x1F1E6;
const FLAG: u32 = 0x1F3F4;
const TAG: u32 = 0xE0000;
const TAG_END: u32 = 0xE007F;
const SUBDIVISIONS: [&str; 3] = ["GB-ENG", "GB-SCT", "GB-WLS"];

type Fallible<T> = Result<T, Box<dyn Error>>;

struct Image {
    width: usize,
    height: usize,
    rgba: Vec<[u8; 4]>,
}

fn read_image(path: &Path) -> Fallible<Image> {
    let mut decoder = png::Decoder::new(std::io::BufReader::new(fs::File::open(path)?));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info()?;
    let mut bytes = vec![0; reader.output_buffer_size().ok_or("a picture too large")?];
    let frame = reader.next_frame(&mut bytes)?;
    let (width, height) = (frame.width as usize, frame.height as usize);
    let rgba: Vec<[u8; 4]> = match frame.color_type {
        png::ColorType::Rgba => bytes.as_chunks::<4>().0.to_vec(),
        png::ColorType::Rgb => bytes.as_chunks().0.iter().map(|&[red, green, blue]| [red, green, blue, 0xFF]).collect(),
        png::ColorType::GrayscaleAlpha => {
            bytes.as_chunks().0.iter().map(|&[grey, alpha]| [grey, grey, grey, alpha]).collect()
        }
        png::ColorType::Grayscale => bytes.iter().map(|grey| [*grey, *grey, *grey, 0xFF]).collect(),
        png::ColorType::Indexed => return Err(format!("{}: a palette left unexpanded", path.display()).into()),
    };
    Ok(Image { width, height, rgba: rgba.into_iter().take(width * height).collect() })
}

// The picture shrunk to fit a square of `side` within its margin, each pixel the mean of what it covers, none where less than half of it is covered.
fn shrink(image: &Image, side: usize) -> Vec<Option<[u8; 3]>> {
    let room = (side - 2 * MARGIN) as f32;
    let scale = room / image.width.max(image.height) as f32;
    let (width, height) =
        ((image.width as f32 * scale).round() as usize, (image.height as f32 * scale).round() as usize);
    let (left, top) = ((side - width) / 2, (side - height) / 2);
    let mut pixels = vec![None; side * side];
    let spans = |at: usize, count: usize, length: usize| {
        let (from, to) = (at as f32 * length as f32 / count as f32, (at + 1) as f32 * length as f32 / count as f32);
        (from.floor() as usize..(to.ceil() as usize).min(length))
            .map(move |source| (source, (to.min(source as f32 + 1.0) - from.max(source as f32)).max(0.0)))
    };
    for y in 0..height {
        for x in 0..width {
            let (mut sum, mut area) = ([0f32; 4], 0f32);
            for (source_y, down) in spans(y, height, image.height) {
                for (source_x, across) in spans(x, width, image.width) {
                    let [red, green, blue, alpha] = image.rgba[source_y * image.width + source_x].map(f32::from);
                    let (weight, cover) = (down * across, alpha / 255.0);
                    sum = [
                        sum[0] + red * cover * weight,
                        sum[1] + green * cover * weight,
                        sum[2] + blue * cover * weight,
                        sum[3] + cover * weight,
                    ];
                    area += weight;
                }
            }
            if area > 0.0 && sum[3] / area >= OPAQUE {
                let channel = |total: f32| (total / sum[3]).round().clamp(0.0, 255.0) as u8;
                pixels[(top + y) * side + left + x] = Some([channel(sum[0]), channel(sum[1]), channel(sum[2])]);
            }
        }
    }
    pixels
}

// The colours a median cut of `pixels` gives, as many as an index of four bits names beside none.
fn to_palette(mut pixels: Vec<[u8; 3]>) -> Vec<[u8; 3]> {
    pixels.sort_unstable();
    let mut boxes = vec![pixels];
    let reach = |pixels: &[[u8; 3]]| {
        (0..3)
            .map(|channel| {
                let values = pixels.iter().map(|pixel| pixel[channel]);
                (values.clone().max().unwrap_or(0) - values.min().unwrap_or(0), channel)
            })
            .max()
            .unwrap_or((0, 0))
    };
    while boxes.len() < COLOURS {
        let widest = (0..boxes.len()).max_by_key(|at| (reach(&boxes[*at]).0, boxes[*at].len(), usize::MAX - at));
        let Some(at) = widest.filter(|at| reach(&boxes[*at]).0 > 0) else {
            break;
        };
        let mut pixels = boxes.swap_remove(at);
        let channel = reach(&pixels).1;
        pixels.sort_by_key(|pixel| (pixel[channel], *pixel));
        let upper = pixels.split_off(pixels.len() / 2);
        boxes.extend([pixels, upper]);
    }
    let mean = |pixels: &Vec<[u8; 3]>| {
        let sum = pixels.iter().fold([0usize; 3], |sum, pixel| [0, 1, 2].map(|at| sum[at] + pixel[at] as usize));
        sum.map(|channel| ((channel + pixels.len() / 2) / pixels.len().max(1)) as u8)
    };
    let mut palette: Vec<[u8; 3]> = boxes.iter().filter(|pixels| !pixels.is_empty()).map(mean).collect();
    palette.sort_unstable();
    palette.dedup();
    palette
}

fn nearest(palette: &[[u8; 3]], pixel: [u8; 3]) -> u8 {
    let distance = |colour: &[u8; 3]| -> u32 {
        (0..3).map(|at| (i32::from(colour[at]) - i32::from(pixel[at])).unsigned_abs().pow(2)).sum()
    };
    palette.iter().enumerate().min_by_key(|(at, colour)| (distance(colour), *at)).map_or(0, |(at, _)| at as u8)
}

// A picture's palette, then its pixels at each side, four bits each, nought for none.
fn to_picture(image: &Image) -> Vec<u8> {
    let shrunk = SIDES.map(|side| shrink(image, side));
    let palette = to_palette(shrunk.iter().flatten().flatten().copied().collect());
    let mut bytes: Vec<u8> = palette.iter().flatten().copied().collect();
    bytes.resize(COLOURS * 3, 0);
    for pixels in &shrunk {
        let index = |pixel: &Option<[u8; 3]>| pixel.map_or(0, |pixel| nearest(&palette, pixel) + 1);
        bytes.extend(pixels.chunks(2).map(|pair| index(&pair[0]) << 4 | index(&pair[1])));
    }
    bytes
}

// A character's number in the bytes a table's record holds it in, highest first.
fn code_bytes(code: u32, bytes: usize) -> Vec<u8> {
    code.to_be_bytes()[size_of::<u32>() - bytes..].to_vec()
}

fn to_key(codes: &[u32]) -> Option<Vec<u8>> {
    let kept: Vec<u32> = codes.iter().copied().filter(|code| *code != u32::from(PICTURE)).collect();
    let is_tone = |code: &u32| char::from_u32(*code).is_some_and(|character| TONES.contains(&character));
    let fits = !kept.is_empty() && kept.len() <= KEY_CODES && !kept.iter().any(is_tone);
    fits.then(|| {
        let mut key: Vec<u8> = kept.iter().flat_map(|code| code_bytes(*code, CODE_BYTES)).collect();
        key.resize(KEY_CODES * CODE_BYTES, 0);
        key
    })
}

fn flag_codes(name: &str) -> Option<Vec<u32>> {
    let letters = || name.bytes().filter(u8::is_ascii_uppercase);
    if name.len() == 2 && letters().count() == 2 {
        return Some(letters().map(|letter| FLAG_LETTER + u32::from(letter - b'A')).collect());
    }
    SUBDIVISIONS.contains(&name).then(|| {
        let tags = letters().map(|letter| TAG + u32::from(letter.to_ascii_lowercase()));
        [FLAG].into_iter().chain(tags).chain([TAG_END]).collect()
    })
}

fn stems(directory: &Path) -> Fallible<Vec<(String, std::path::PathBuf)>> {
    let mut files = Vec::new();
    for entry in fs::read_dir(directory)? {
        let path = entry?.path();
        if path.extension().is_some_and(|extension| extension == "png") {
            let stem = path.file_stem().and_then(|stem| stem.to_str()).ok_or("a file name that is no text")?;
            files.push((stem.to_string(), path.clone()));
        }
    }
    files.sort();
    Ok(files)
}

// Every emoji's key with its picture: Noto's own by the numbers in their names, and the flags by their regions.
fn read_emoji(noto: &Path) -> Fallible<BTreeMap<Vec<u8>, Vec<u8>>> {
    let mut emoji = BTreeMap::new();
    for (stem, path) in stems(&noto.join("2D/png/72"))? {
        let numbers = stem.strip_prefix("emoji_u").ok_or_else(|| format!("{stem}: no emoji's name"))?;
        let codes: Vec<u32> = numbers.split('_').map(|code| u32::from_str_radix(code, 16)).collect::<Result<_, _>>()?;
        if let Some(key) = to_key(&codes) {
            emoji.insert(key, to_picture(&read_image(&path)?));
        }
    }
    for (stem, path) in stems(&noto.join("third_party/region-flags/png"))? {
        if let Some(key) = flag_codes(&stem).and_then(|codes| to_key(&codes)) {
            emoji.insert(key, to_picture(&read_image(&path)?));
        }
    }
    Ok(emoji)
}

fn single(key: &[u8]) -> Option<u32> {
    let code = |at: usize| key[at..at + CODE_BYTES].iter().fold(0u32, |code, byte| code << u8::BITS | u32::from(*byte));
    (code(CODE_BYTES) == 0).then(|| code(0))
}

fn read_property(file: &str, property: &str) -> Fallible<BTreeSet<u32>> {
    let mut codes = BTreeSet::new();
    for line in file.lines() {
        let mut fields = line.split('#').next().unwrap_or("").split(';').map(str::trim);
        if let (Some(range), Some(name)) = (fields.next(), fields.next())
            && name == property
        {
            let mut ends = range.split("..").map(|end| u32::from_str_radix(end, 16));
            let first = ends.next().ok_or("a line without a number")??;
            codes.extend(first..=ends.next().transpose()?.unwrap_or(first));
        }
    }
    Ok(codes)
}

// Unifont's drawings but those of `left_out`: the count of the narrow ones and of the wide ones, then each kind by its number.
fn to_drawings(hex: &str, left_out: &BTreeSet<u32>) -> Fallible<Vec<u8>> {
    let (mut narrow, mut wide) = (BTreeMap::new(), BTreeMap::new());
    let [narrow_bytes, wide_bytes] = unifont::WIDTHS.map(|width| unifont::ROWS * (width / u8::BITS) as usize);
    let is_private = |code: u32| {
        char::from_u32(code)
            .is_some_and(|character| unifont::PRIVATE.iter().any(|private| private.contains(&character)))
    };
    for line in hex.lines() {
        let (code, digits) = line.split_once(':').ok_or("a line of Unifont's without its colon")?;
        let code = u32::from_str_radix(code, 16)?;
        if left_out.contains(&code) || is_private(code) {
            continue;
        }
        let rows: Vec<u8> = (0..digits.len() / 2)
            .map(|at| u8::from_str_radix(&digits[2 * at..2 * at + 2], 16))
            .collect::<Result<_, _>>()?;
        match rows.len() {
            length if length == narrow_bytes => narrow.insert(code, rows),
            length if length == wide_bytes => wide.insert(code, rows),
            length => return Err(format!("{code:04X}: a drawing of {length} bytes").into()),
        };
    }
    let mut bytes = Vec::new();
    bytes.extend((narrow.len() as u32).to_le_bytes());
    bytes.extend((wide.len() as u32).to_le_bytes());
    for (code, rows) in narrow.iter().chain(&wide) {
        bytes.extend(code_bytes(*code, unifont::CODE_BYTES));
        bytes.extend(rows);
    }
    Ok(bytes)
}

fn main() -> Fallible<()> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let [unifont, emoji_data, noto, assets] = arguments.as_slice() else {
        return Err("usage: glyphs UNIFONT.hex EMOJI-DATA.txt NOTO-EMOJI ASSETS".into());
    };
    let assets = Path::new(assets);
    let spleen: BTreeSet<u32> = font::spleen_characters().map(u32::from).collect();
    let shown = read_property(&fs::read_to_string(emoji_data)?, "Emoji_Presentation")?;
    let emoji = read_emoji(Path::new(noto))?;
    let mut bare = BTreeSet::new();
    let mut pictures = (emoji.len() as u32).to_le_bytes().to_vec();
    for (key, picture) in &emoji {
        let is_bare = single(key).is_some_and(|code| shown.contains(&code));
        if let Some(code) = single(key).filter(|_| is_bare) {
            if spleen.contains(&code) {
                return Err(format!("{code:04X}: an emoji shown by itself that Spleen draws").into());
            }
            bare.insert(code);
        }
        pictures.extend(key);
        pictures.push(u8::from(is_bare));
        pictures.extend(picture);
    }
    let left_out: BTreeSet<u32> = spleen.union(&bare).copied().collect();
    let drawings = to_drawings(&fs::read_to_string(unifont)?, &left_out)?;
    fs::create_dir_all(assets.join("noto"))?;
    fs::write(assets.join("noto/emoji.bin"), &pictures)?;
    fs::write(assets.join("unifont/glyphs.bin"), &drawings)?;
    println!("{} emoji in {} bytes, Unifont's drawings in {} bytes", emoji.len(), pictures.len(), drawings.len());
    Ok(())
}
