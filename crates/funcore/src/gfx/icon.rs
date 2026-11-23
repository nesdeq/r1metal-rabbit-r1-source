//! Vector icons: SVG paths as outlines of short straight pieces.

use crate::base::math::sqrt;
use alloc::vec::Vec;

const PIECE: f32 = 1.0 / 96.0;
const FLAT: f32 = 1.0 / 8192.0;
const VIEW_BOX: &str = "viewBox=\"";
const PATH: &str = "<path";
const PATH_END: char = '>';
const PATH_DATA: &str = " d=\"";
const PATH_NAME: &str = " id=\"";
const QUOTE: char = '"';
const FEWEST_POINTS: usize = 3;

/// An outline point in the icon's box, 0 to 1 from the top left.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Error {
    NoViewBox,
    NoPath,
    BadNumber,
    Unsupported(char),
    StrayNumber,
    NoStart,
}

#[derive(Clone, Copy)]
struct View {
    left: f32,
    top: f32,
    width: f32,
    height: f32,
}

struct Scanner<'a> {
    rest: &'a str,
}

impl Scanner<'_> {
    fn skip_separators(&mut self) {
        self.rest = self.rest.trim_start_matches(|character: char| character.is_ascii_whitespace() || character == ',');
    }

    fn is_done(&mut self) -> bool {
        self.skip_separators();
        self.rest.is_empty()
    }

    fn command(&mut self) -> Option<char> {
        self.skip_separators();
        let letter = self.rest.chars().next().filter(char::is_ascii_alphabetic)?;
        self.rest = &self.rest[letter.len_utf8()..];
        Some(letter)
    }

    fn number(&mut self) -> Result<f32, Error> {
        self.skip_separators();
        let bytes = self.rest.as_bytes();
        let mut end = usize::from(matches!(bytes.first(), Some(b'+' | b'-')));
        let mut has_point = false;
        while let Some(&byte) = bytes.get(end) {
            match byte {
                b'0'..=b'9' => {}
                b'.' if !has_point => has_point = true,
                _ => break,
            }
            end += 1;
        }
        let (text, rest) = self.rest.split_at(end);
        self.rest = rest;
        text.parse().map_err(|_| Error::BadNumber)
    }
}

fn attribute<'a>(svg: &'a str, opening: &str) -> Option<&'a str> {
    let start = svg.find(opening)? + opening.len();
    let length = svg[start..].find(QUOTE)?;
    Some(&svg[start..start + length])
}

fn paths(svg: &str) -> impl Iterator<Item = &str> {
    svg.split(PATH).skip(1).map(|rest| rest.split(PATH_END).next().unwrap_or(rest))
}

fn view(svg: &str) -> Result<View, Error> {
    let mut scanner = Scanner { rest: attribute(svg, VIEW_BOX).ok_or(Error::NoViewBox)? };
    let view =
        View { left: scanner.number()?, top: scanner.number()?, width: scanner.number()?, height: scanner.number()? };
    if view.width > 0.0 && view.height > 0.0 { Ok(view) } else { Err(Error::NoViewBox) }
}

fn span(a: Point, b: Point) -> f32 {
    (b.x - a.x).abs().max((b.y - a.y).abs())
}

fn pieces(length: f32) -> usize {
    (length / PIECE) as usize + 1
}

#[derive(Clone, Copy)]
struct Control {
    at: Point,
    is_cubic: bool,
}

struct Tracer {
    contours: Vec<Vec<Point>>,
    start: Point,
    at: Point,
}

impl Tracer {
    fn begin(&mut self, at: Point) {
        self.contours.push(Vec::from([at]));
        self.start = at;
        self.at = at;
    }

    fn push(&mut self, point: Point) -> Result<(), Error> {
        self.contours.last_mut().ok_or(Error::NoStart)?.push(point);
        Ok(())
    }

    fn line(&mut self, to: Point) -> Result<(), Error> {
        let from = self.at;
        let count = pieces(span(from, to));
        for index in 1..=count {
            let t = index as f32 / count as f32;
            self.push(Point { x: from.x + (to.x - from.x) * t, y: from.y + (to.y - from.y) * t })?;
        }
        self.at = to;
        Ok(())
    }

    fn quadratic(&mut self, control: Point, to: Point) -> Result<(), Error> {
        let from = self.at;
        let (bend_x, bend_y) = (from.x - 2.0 * control.x + to.x, from.y - 2.0 * control.y + to.y);
        let flat = sqrt(sqrt(bend_x * bend_x + bend_y * bend_y) / (4.0 * FLAT)) as usize + 1;
        let count = pieces(span(from, control) + span(control, to)).max(flat);
        for index in 1..=count {
            let t = index as f32 / count as f32;
            let u = 1.0 - t;
            self.push(Point {
                x: u * u * from.x + 2.0 * u * t * control.x + t * t * to.x,
                y: u * u * from.y + 2.0 * u * t * control.y + t * t * to.y,
            })?;
        }
        self.at = to;
        Ok(())
    }

    fn cubic(&mut self, first: Point, second: Point, to: Point) -> Result<(), Error> {
        let from = self.at;
        let bend = |a: Point, b: Point, c: Point| {
            let (x, y) = (a.x - 2.0 * b.x + c.x, a.y - 2.0 * b.y + c.y);
            sqrt(x * x + y * y)
        };
        let most = bend(from, first, second).max(bend(first, second, to));
        let flat = sqrt(3.0 * most / (4.0 * FLAT)) as usize + 1;
        let count = pieces(span(from, first) + span(first, second) + span(second, to)).max(flat);
        for index in 1..=count {
            let t = index as f32 / count as f32;
            let u = 1.0 - t;
            let (a, b, c, d) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
            self.push(Point {
                x: a * from.x + b * first.x + c * second.x + d * to.x,
                y: a * from.y + b * first.y + c * second.y + d * to.y,
            })?;
        }
        self.at = to;
        Ok(())
    }

    fn path(data: &str, view: View) -> Result<Vec<Vec<Point>>, Error> {
        let mut scanner = Scanner { rest: data };
        let origin = Point { x: -view.left / view.width, y: -view.top / view.height };
        let mut tracer = Tracer { contours: Vec::new(), start: origin, at: origin };
        let mut command = None;
        let mut control = None;
        while !scanner.is_done() {
            let letter = match (scanner.command(), command) {
                (Some(letter), _) => letter,
                (None, Some('M')) => 'L',
                (None, Some('m')) => 'l',
                (None, Some(repeated)) if !repeated.eq_ignore_ascii_case(&'z') => repeated,
                (None, _) => return Err(Error::StrayNumber),
            };
            command = Some(letter);
            let from = tracer.at;
            let relative = letter.is_ascii_lowercase();
            let x = |scanner: &mut Scanner| -> Result<f32, Error> {
                let value = scanner.number()? / view.width;
                Ok(if relative { from.x + value } else { origin.x + value })
            };
            let y = |scanner: &mut Scanner| -> Result<f32, Error> {
                let value = scanner.number()? / view.height;
                Ok(if relative { from.y + value } else { origin.y + value })
            };
            let point =
                |scanner: &mut Scanner| -> Result<Point, Error> { Ok(Point { x: x(scanner)?, y: y(scanner)? }) };
            // SVG: a smooth curve mirrors the last control point of the curve before it where that was of its own kind, else starts at the pen.
            let mirror = |last: Option<Control>, is_cubic: bool| match last {
                Some(Control { at, is_cubic: was_cubic }) if was_cubic == is_cubic => {
                    Point { x: 2.0 * from.x - at.x, y: 2.0 * from.y - at.y }
                }
                _ => from,
            };
            let last = control.take();
            match letter.to_ascii_uppercase() {
                'M' => tracer.begin(point(&mut scanner)?),
                'L' => tracer.line(point(&mut scanner)?)?,
                'H' => tracer.line(Point { x: x(&mut scanner)?, y: from.y })?,
                'V' => tracer.line(Point { x: from.x, y: y(&mut scanner)? })?,
                curve @ ('Q' | 'T') => {
                    let via = if curve == 'T' { mirror(last, false) } else { point(&mut scanner)? };
                    tracer.quadratic(via, point(&mut scanner)?)?;
                    control = Some(Control { at: via, is_cubic: false });
                }
                curve @ ('C' | 'S') => {
                    let first = if curve == 'S' { mirror(last, true) } else { point(&mut scanner)? };
                    let second = point(&mut scanner)?;
                    tracer.cubic(first, second, point(&mut scanner)?)?;
                    control = Some(Control { at: second, is_cubic: true });
                }
                'Z' => {
                    let start = tracer.start;
                    tracer.line(start)?;
                }
                _ => return Err(Error::Unsupported(letter)),
            }
        }
        Ok(tracer.contours)
    }
}

/// An icon's outlines, filled by the nonzero rule.
#[derive(Clone)]
pub struct Icon {
    contours: Vec<Vec<Point>>,
}

pub(crate) fn extent<'a>(points: impl IntoIterator<Item = &'a Point>) -> (Point, Point) {
    let mut points = points.into_iter();
    let first = points.next().copied().unwrap_or(Point { x: 0.0, y: 0.0 });
    points.fold((first, first), |(low, high), point| {
        (
            Point { x: low.x.min(point.x), y: low.y.min(point.y) },
            Point { x: high.x.max(point.x), y: high.y.max(point.y) },
        )
    })
}

impl Icon {
    fn read(svg: &str, is_included: impl Fn(&str) -> bool) -> Result<Icon, Error> {
        let view = view(svg)?;
        let mut contours = Vec::new();
        let mut has_path = false;
        for data in paths(svg).filter(|tag| is_included(tag)).filter_map(|tag| attribute(tag, PATH_DATA)) {
            contours.append(&mut Tracer::path(data, view)?);
            has_path = true;
        }
        if !has_path {
            return Err(Error::NoPath);
        }
        contours.retain(|contour| contour.len() >= FEWEST_POINTS);
        Ok(Icon { contours })
    }

    /// Reads the `viewBox` and every path of an SVG file.
    pub fn from_svg(svg: &str) -> Result<Icon, Error> {
        Icon::read(svg, |_| true)
    }

    pub(crate) fn part(svg: &str, name: &str) -> Result<Icon, Error> {
        Icon::read(svg, |tag| attribute(tag, PATH_NAME) == Some(name))
    }

    pub(crate) fn with_contours(contours: Vec<Vec<Point>>) -> Icon {
        Icon { contours }
    }

    /// Every outline as a closed run of points.
    pub fn contours(&self) -> &[Vec<Point>] {
        &self.contours
    }

    pub(crate) fn points(&self) -> usize {
        self.contours.iter().map(Vec::len).sum()
    }

    pub(crate) fn extent(&self) -> Option<(Point, Point)> {
        self.contours.iter().any(|contour| !contour.is_empty()).then(|| extent(self.contours.iter().flatten()))
    }

    pub(crate) fn fit(&self, around: (Point, Point), share: f32) -> Icon {
        let (low, high) = around;
        let longer = (high.x - low.x).max(high.y - low.y);
        if longer <= 0.0 {
            return Icon { contours: self.contours.clone() };
        }
        let (scale, middle) = (share / longer, Point { x: (low.x + high.x) / 2.0, y: (low.y + high.y) / 2.0 });
        let place =
            |point: &Point| Point { x: 0.5 + (point.x - middle.x) * scale, y: 0.5 + (point.y - middle.y) * scale };
        Icon { contours: self.contours.iter().map(|contour| contour.iter().map(place).collect()).collect() }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::super::support::close;
    use super::*;
    use alloc::format;
    use alloc::string::String;
    use std::path::{Path, PathBuf};
    use std::string::ToString;

    fn svg(path: &str) -> String {
        format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" height=\"48\" viewBox=\"0 -960 960 960\" width=\"48\"><path d=\"{path}\"/></svg>"
        )
    }

    fn doubled_area(icon: &Icon) -> f32 {
        let around = |contour: &Vec<Point>| -> f32 {
            let after = contour.iter().cycle().skip(1);
            contour.iter().zip(after).map(|(a, b)| a.x * b.y - b.x * a.y).sum()
        };
        icon.contours().iter().map(around).sum()
    }

    #[test]
    fn square_area() {
        let icon = Icon::from_svg(&svg("M240-720h480v480H240Z")).unwrap();
        assert_eq!(icon.contours().len(), 1);
        assert!(close(doubled_area(&icon).abs(), 0.5));
        let corner = icon.contours()[0][0];
        assert!(close(corner.x, 0.25) && close(corner.y, 0.25));
    }

    #[test]
    fn path_spellings_agree() {
        for (one, other, what) in [
            ("M100-900L500-900L500-500Q300-500 100-700Z", "m100-900l400 0l0 400q-200 0-400-200z", "relative commands"),
            (
                "M0-960h.5.5v480.5.5h-1-.5z",
                "M 0 -960 h 0.5 h 0.5 v 480.5 v 0.5 h -1 h -0.5 z",
                "numbers packed together",
            ),
            ("M100-100 500-100 500-500z", "M100-100L500-100L500-500z", "coordinates after a move, which are lines"),
            ("M0-500q100-200 200 0t200 0V-100H0Z", "M0-500q100-200 200 0q100 200 200 0V-100H0Z", "a smooth quadratic"),
            // A quadratic from 0 over 200 to 400 is the cubic whose control points lie two thirds of the way to its own.
            (
                "M0-500Q200-900 400-500V-100H0Z",
                "M0-500C133.333-766.667 266.667-766.667 400-500V-100H0Z",
                "a raised cubic",
            ),
            (
                "M0-500c100-200 300-200 400 0s300 200 400 0V-100H0Z",
                "M0-500c100-200 300-200 400 0c100 200 300 200 400 0V-100H0Z",
                "a smooth cubic, which mirrors the cubic before it",
            ),
            (
                "M0-500Q200-900 400-500S700-100 800-500V-100H0Z",
                "M0-500Q200-900 400-500C400-500 700-100 800-500V-100H0Z",
                "a smooth cubic after a quadratic, which starts at the pen",
            ),
        ] {
            let area = |path: &str| doubled_area(&Icon::from_svg(&svg(path)).unwrap());
            assert!(close(area(one), area(other)) && area(one).abs() > 0.0, "{what}");
        }
    }

    #[test]
    fn move_after_close() {
        let icon = Icon::from_svg(&svg("M100-900h200v200h-200zm400 0h200v200h-200z")).unwrap();
        let (low, _) = icon.extent().unwrap();
        assert!(close(low.x, 100.0 / 960.0));
        let second = icon.contours()[1][0];
        assert!(close(second.x, 500.0 / 960.0) && close(second.y, 60.0 / 960.0), "{second:?}");
    }

    #[test]
    fn short_pieces_on_curve() {
        let icon = Icon::from_svg(&svg("M0-960H960V0H0Z")).unwrap();
        let contour = &icon.contours()[0];
        let after = contour.iter().cycle().skip(1);
        assert!(contour.iter().zip(after).all(|(a, b)| span(*a, *b) <= PIECE + 1e-6));
        let disc = Icon::from_svg(&svg(
            "M480-880q166 0 283 117t117 283q0 166-117 283T480-80q-166 0-283-117T80-480q0-166 117-283t283-117Z",
        ))
        .unwrap();
        let (middle, radius) = (Point { x: 0.5, y: 0.5 }, 400.0 / 960.0);
        let contour = &disc.contours()[0];
        let after = contour.iter().cycle().skip(1);
        for (a, b) in contour.iter().zip(after) {
            let half = Point { x: (a.x + b.x) / 2.0, y: (a.y + b.y) / 2.0 };
            let far = sqrt((half.x - middle.x) * (half.x - middle.x) + (half.y - middle.y) * (half.y - middle.y));
            assert!(
                radius - far < 2.0 * FLAT + 0.01 * radius,
                "a piece's middle within the curve's own way off a circle"
            );
        }
    }

    #[test]
    fn empty_outlines_dropped() {
        let icon = Icon::from_svg(&svg("M480-651ZM240-720h480v480H240Zm0 0v-480 480Z")).unwrap();
        assert!(close(doubled_area(&icon).abs(), 0.5));
        assert!(icon.contours().len() <= 2);
    }

    #[test]
    fn extent_of_all_outlines() {
        let icon = Icon::from_svg(&svg("M240-720h240v240H240ZM600-360h120v240H600Z")).unwrap();
        let (low, high) = icon.extent().unwrap();
        assert!(close(low.x, 0.25) && close(low.y, 0.25));
        assert!(close(high.x, 0.75) && close(high.y, 0.875));
        assert_eq!(Icon::from_svg(&svg("M480-651Z")).unwrap().extent(), None);
    }

    #[test]
    fn fit_centres_at_share() {
        let icon = Icon::from_svg(&svg("M0-960h960v240H0ZM100-900h100v100H100Z")).unwrap();
        let own = icon.extent().unwrap();
        let fitted = icon.fit(own, 0.5);
        let (low, high) = fitted.extent().unwrap();
        assert!(
            close(low.x, 0.25) && close(high.x, 0.75) && close(low.y, 0.4375) && close(high.y, 0.5625),
            "a wide one: half the box across, and as much lower as it was"
        );
        assert_eq!(fitted.contours().len(), 2, "holes and all");
        assert!(close(doubled_area(&fitted) / doubled_area(&icon), 0.25), "every outline by the same measure");
        let tall = Icon::from_svg(&svg("M600-600h60v360h-60Z")).unwrap();
        let (low, high) = tall.fit(tall.extent().unwrap(), 0.75).extent().unwrap();
        assert!(
            close(low.y, 0.125) && close(high.y, 0.875) && close(low.x, 0.4375) && close(high.x, 0.5625),
            "a small tall one grows, and comes to the middle"
        );
        let (low, high) = tall.fit(own, 0.5).extent().unwrap();
        assert!(
            close(high.y - low.y, 0.1875) && close((low.x + high.x) / 2.0, 0.578_125),
            "fitted as another icon is, it keeps its size and place beside that one"
        );
        let nothing = Icon::with_contours(Vec::new());
        assert_eq!(nothing.fit((Point { x: 0.0, y: 0.0 }, Point { x: 0.0, y: 0.0 }), 0.5).extent(), None);
    }

    #[test]
    fn paths_and_named_parts() {
        let drawing = "<svg viewBox=\"0 0 8 8\">\n<path id=\"box\" d=\"M0 0H4V4H0Z\"/>\n<path id=\"pane\" d=\"M4 4H8V8H4ZM5 5V7H7V5Z\"/>\n<path id=\"none\"/><path d=\"M0 4H2V8H0Z\"/></svg>";
        let whole = Icon::from_svg(drawing).unwrap();
        assert_eq!(whole.contours().len(), 4);
        assert!(close(doubled_area(&whole).abs(), 2.0 * (0.25 + 0.25 - 0.0625 + 0.125)));
        let (pane, around) =
            (Icon::part(drawing, "pane").unwrap(), Icon::part(drawing, "box").unwrap().extent().unwrap());
        assert_eq!(pane.contours().len(), 2, "an outline and the hole in it");
        assert!(close(doubled_area(&pane).abs(), 2.0 * (0.25 - 0.0625)));
        assert!(close(around.0.x, 0.0) && close(around.1.x, 0.5) && close(around.1.y, 0.5));
        assert_eq!(
            [Icon::part(drawing, "pan").err(), Icon::part(drawing, "none").err()],
            [Some(Error::NoPath); 2],
            "no path of that name, and one that draws nothing"
        );
        assert_eq!(
            Icon::from_svg("<svg viewBox=\"0 0 8 8\"><path d=\"M0 0H4V4\"/><path d=\"H8\"/></svg>").err(),
            Some(Error::NoStart),
            "every path begins with a move of its own"
        );
    }

    const SIDE: usize = 254;
    const FINER: usize = 8;
    const OFF: u8 = 16;

    fn covered(icon: &Icon, side: usize) -> Vec<u8> {
        let mut mask = super::super::mask::Mask::new(side, side);
        for contour in icon.contours() {
            mask.outline(contour.iter().map(|point| (point.x * side as f32, point.y * side as f32)));
        }
        mask.fill();
        (0..side)
            .flat_map(|row| mask.coverage(row).map(|share| (share * 255.0 + 0.5) as u8).collect::<Vec<u8>>())
            .collect()
    }

    fn truth(file: &Path, side: usize) -> Vec<u8> {
        let large = side * FINER;
        let drawn = std::process::Command::new("rsvg-convert")
            .args(["-w", &large.to_string(), "-h", &large.to_string()])
            .arg(file)
            .output()
            .expect("librsvg's rsvg-convert");
        assert!(drawn.status.success(), "{}", file.display());
        let mut reader = png::Decoder::new(std::io::Cursor::new(drawn.stdout)).read_info().expect("a PNG");
        let mut pixels = alloc::vec![0; reader.output_buffer_size().expect("a size")];
        let frame = reader.next_frame(&mut pixels).expect("a picture");
        assert_eq!((frame.width as usize, frame.color_type), (large, png::ColorType::Rgba));
        let alpha = |x: usize, y: usize| u32::from(pixels[(y * large + x) * 4 + 3]);
        (0..side * side)
            .map(|at| {
                let (x, y) = (at % side * FINER, at / side * FINER);
                let sum: u32 = (0..FINER)
                    .flat_map(|down| (0..FINER).map(move |across| (across, down)))
                    .map(|(across, down)| alpha(x + across, y + down))
                    .sum();
                ((sum + (FINER * FINER / 2) as u32) / (FINER * FINER) as u32) as u8
            })
            .collect()
    }

    #[test]
    #[ignore = "has the host's librsvg draw every icon of Material Symbols and the older icons the interface draws eight times as large, the truth each is held to"]
    fn material_symbols_match_librsvg() {
        let assets = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets");
        let mut files: Vec<PathBuf> = std::fs::read_dir(assets.join("material"))
            .expect("the icons of Material Symbols")
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ending| ending == "svg"))
            .collect();
        assert!(files.len() > 3000, "the whole set: {}", files.len());
        files.push(assets.join("material-icons").join("catching_pokemon.svg"));
        files.sort();
        let threads = std::thread::available_parallelism().map_or(1, |count| count.get());
        let worst: Vec<(u8, String)> = std::thread::scope(|scope| {
            let workers: Vec<_> = files
                .chunks(files.len().div_ceil(threads))
                .map(|share| {
                    scope.spawn(move || {
                        share
                            .iter()
                            .map(|file| {
                                let svg = std::fs::read_to_string(file).expect("a file of text");
                                let icon = Icon::from_svg(&svg).unwrap_or_else(|e| panic!("{}: {e:?}", file.display()));
                                let off = covered(&icon, SIDE)
                                    .iter()
                                    .zip(truth(file, SIDE))
                                    .map(|(ours, true_one)| ours.abs_diff(true_one))
                                    .max()
                                    .unwrap_or(0);
                                (
                                    off,
                                    file.file_stem()
                                        .map(|stem| stem.to_string_lossy().into_owned())
                                        .unwrap_or_default(),
                                )
                            })
                            .collect::<Vec<_>>()
                    })
                })
                .collect();
            workers.into_iter().flat_map(|worker| worker.join().expect("a worker")).collect()
        });
        let most = worst.iter().max().cloned().unwrap_or_default();
        let past: Vec<&(u8, String)> = worst.iter().filter(|(off, _)| *off > OFF).collect();
        assert!(past.is_empty(), "icons with a pixel off by more than {OFF} 255ths: {past:?}");
        std::println!("{} icons, the most any pixel is off: {} 255ths, in {}", worst.len(), most.0, most.1);
    }

    #[test]
    fn svg_errors() {
        assert_eq!(Icon::from_svg("<svg><path d=\"M0 0\"/></svg>").err(), Some(Error::NoViewBox));
        assert_eq!(Icon::from_svg("<svg viewBox=\"0 0 0 10\"><path d=\"M0 0\"/></svg>").err(), Some(Error::NoViewBox));
        assert_eq!(Icon::from_svg("<svg viewBox=\"0 0 10 10\"></svg>").err(), Some(Error::NoPath));
        assert_eq!(Icon::from_svg(&svg("M0 0A5 5 0 0 1 9 9")).err(), Some(Error::Unsupported('A')));
        assert_eq!(Icon::from_svg(&svg("L5 5")).err(), Some(Error::NoStart));
        assert_eq!(Icon::from_svg(&svg("M0 0z5 5")).err(), Some(Error::StrayNumber));
        assert_eq!(Icon::from_svg(&svg("5 5")).err(), Some(Error::StrayNumber));
        assert_eq!(Icon::from_svg(&svg("M0 -")).err(), Some(Error::BadNumber));
    }
}
