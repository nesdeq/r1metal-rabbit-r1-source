mod items;
mod stills;
mod video;

use crate::crew::Crew;
use crate::media::vdec::{Decoder, StuckList};
use crate::storage::Data;
use crate::sync::Board;
use crate::sys::{self, lock};
use crate::{clock, log};
use items::{delete, list_items};
use r1_funcore::base::pixel::PixelFormat;
use r1_funcore::codec::h264::read::Sequence;
use r1_funcore::domain::gallery::items::Item;
use r1_funcore::domain::gallery::roll::{Batch, Grid, Roll, RollStep, RoundThumbnail};
use r1_funcore::domain::gallery::show::ClockReport;
use r1_funcore::domain::gallery::state::{Job, Report, State};
use r1_funcore::domain::gallery::{
    self, Errand, Kind, NO_PIXEL_PHOTO, Note, RollRequest, Sending, Stamp, Thumbnail, count_rolls,
};
use r1_funcore::domain::magic::Order;
use r1_funcore::gfx::Picture;
use r1_funcore::hw::CORES;
use r1_funcore::input::pose::Pose;
use std::io;
use std::mem;
use std::os::fd::RawFd;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use stills::{DecodeBuffers, read_thumbnail, show_pictures};
use video::{read_poster, show_videos};

// What the gallery's thread decodes with while a roll shows: its crew, the buffers of the photos it decodes, and the video decoder kept from one video to the next it fits.
struct Decoders {
    crew: Crew,
    buffers: Mutex<Vec<DecodeBuffers>>,
    decoder: Mutex<Option<Decoder>>,
    stuck_list: StuckList,
}

impl Decoders {
    fn new(crew: Crew) -> Decoders {
        Decoders { crew, buffers: Mutex::new(Vec::new()), decoder: Mutex::new(None), stuck_list: StuckList::default() }
    }

    fn take_buffers(&self) -> DecodeBuffers {
        lock(&self.buffers).pop().unwrap_or_default()
    }

    fn put_buffers(&self, buffers: DecodeBuffers) {
        lock(&self.buffers).push(buffers);
    }

    fn open_video(&self, sequence: &Sequence, spare: usize) -> io::Result<Decoder> {
        self.stuck_list.retry();
        let decoder = lock(&self.decoder).take().filter(|decoder| decoder.fits(sequence));
        match decoder {
            Some(mut decoder) => {
                decoder.reset();
                Ok(decoder)
            }
            None => Decoder::open(sequence, spare, &self.stuck_list),
        }
    }

    fn keep_video(&self, decoder: Decoder) {
        if decoder.is_at_rest() {
            *lock(&self.decoder) = Some(decoder);
        }
    }

    fn clear(&self) {
        lock(&self.buffers).clear();
        *lock(&self.decoder) = None;
        self.stuck_list.retry();
    }
}

#[derive(Clone, Copy)]
struct Context<'a> {
    data: &'a Data,
    format: PixelFormat,
    refresh_ns: u64,
    decoders: &'a Decoders,
}

struct Shared {
    board: Arc<Board<State>>,
    data: Data,
    refresh_ns: u64,
}

impl Shared {
    fn new(data: Data, refresh_ns: u64) -> io::Result<Shared> {
        Ok(Shared { board: Arc::new(Board::new(State::default())?), data, refresh_ns })
    }

    fn order(&self, order: Order) {
        self.board.lock().report.orders.push(order);
        self.board.raise();
    }

    fn set_note(&self, note: Note) {
        self.board.lock().report.note = Some(note);
        self.board.raise();
    }

    fn send(&self, sending: Sending) {
        self.board.lock().report.send = Some(sending);
        self.board.raise();
    }

    fn copy(&self, photo: &str) {
        self.board.lock().report.copies.push(photo.into());
        self.board.raise();
    }

    fn set_count(&self, kind: Kind, count: usize, nothing: Option<Note>) {
        self.board.lock().set_count(kind, count, nothing);
        self.board.raise();
    }

    fn add_thumbnails(&self, thumbnails: Vec<RoundThumbnail>) {
        self.board.lock().report.thumbnails.extend(thumbnails);
        self.board.raise();
    }

    fn open_item(&self, kind: Kind, count: usize) -> Option<(RollRequest, usize)> {
        self.board.lock().open_item(kind, count)
    }

    fn show(&self, picture: &mut Picture, kind: Kind, shown: (usize, &str)) {
        self.board.lock().put_picture(picture, kind, shown);
        self.board.raise();
    }

    fn show_unreadable(&self, kind: Kind, shown: (usize, &str)) {
        self.show(&mut Picture::default(), kind, shown);
        self.set_note(Note::Unreadable);
    }

    fn play(&self) {
        self.board.lock().show.play();
        self.board.raise();
    }

    fn hold(&self, shown: (usize, &str)) -> ClockReport {
        let (clock, newest) = self.board.lock().show.hold();
        if let Some(mut frame) = newest {
            self.show(&mut frame.picture, Kind::Video, shown);
            self.board.lock().show.give_back(frame.picture);
        }
        self.board.raise();
        clock
    }

    fn clear(&self) {
        let pictures = self.board.lock().show.clear();
        self.board.raise();
        drop(pictures);
    }

    fn rest(&self) {
        let thumbnails = mem::take(&mut self.board.lock().report.thumbnails);
        drop(thumbnails);
        self.board.lock().rest();
        self.board.raise();
    }
}

// The small picture `read` makes of the file `name`, which shows for the item whose file as taken is `file`.
fn make_thumbnail(name: &str, file: &str, read: impl FnOnce() -> io::Result<Thumbnail>) -> Thumbnail {
    let start = Instant::now();
    match read() {
        Ok(thumbnail) => {
            log!("{}", gallery::to_thumbnail_line(name, &thumbnail.picture, start.elapsed().as_millis()));
            thumbnail
        }
        Err(e) => {
            log!("gallery: {name}: {e}");
            Thumbnail::unreadable(file)
        }
    }
}

fn make_thumbnails(context: Context, roll: &Roll, batch: Batch, grid: Grid) -> Vec<(usize, Thumbnail)> {
    let file_of = |item: usize| roll.items[item].original.as_str();
    match batch {
        Batch::Video(item) => {
            let name = roll.view(item).0;
            vec![(item, make_thumbnail(name, file_of(item), || read_poster(context, name, grid.size)))]
        }
        Batch::Photos(items) => {
            let mut thumbnails: Vec<Option<Thumbnail>> = items.iter().map(|_| None).collect();
            context.decoders.crew.for_each(&mut thumbnails, |at, thumbnail| {
                let ((name, label), file) = (roll.view(items[at]), file_of(items[at]));
                let read = || read_thumbnail(context, (name, file), label, grid.size);
                *thumbnail = Some(make_thumbnail(name, file, read));
            });
            // A picture whose thread did not come back with it counts as one that cannot be read.
            let pairs = items.into_iter().zip(thumbnails);
            pairs
                .map(|(item, thumbnail)| (item, thumbnail.unwrap_or_else(|| Thumbnail::unreadable(file_of(item)))))
                .collect()
        }
    }
}

fn run_errand(shared: &Shared, kind: Kind, (item, stamp, name): (&Item, Stamp, &str)) -> Result<(), Note> {
    match stamp {
        Stamp::Send => {
            let sending = match kind {
                Kind::Photo => stills::photo_size(&shared.data, name).map(|size| Sending::Photo(name.into(), size)),
                Kind::Video => video::bytes_of(&shared.data, name).map(|bytes| Sending::Video(name.into(), bytes)),
            };
            sending.map(|sending| shared.send(sending)).map_err(|e| {
                log!("gallery: {name} is not sent: {e}");
                stamp.failure()
            })
        }
        Stamp::Trash => delete(shared, item, kind).map(|()| log!("{}", gallery::to_deleted_line(name))).map_err(|e| {
            log!("gallery: {name} is not deleted: {e}");
            stamp.failure()
        }),
        Stamp::Redraw => {
            let photo = item.photo().ok_or_else(|| io::Error::other(NO_PIXEL_PHOTO));
            photo.map(|photo| shared.copy(photo)).map_err(|e| {
                log!("gallery: {name} is not redrawn anew: {e}");
                stamp.failure()
            })
        }
    }
}

fn count(shared: &Shared) {
    let data = Some(&shared.data).filter(|data| data.wait_settled());
    let tally = count_rolls(&list_items(data, Kind::Photo), &list_items(data, Kind::Video), clock::local());
    shared.board.lock().take_count(tally);
    shared.board.raise();
}

fn run_errands(roll: &Roll, shared: &Shared, errands: Vec<Errand>) {
    for errand in roll.errands(errands) {
        if let Err(note) = run_errand(shared, roll.kind, errand) {
            shared.set_note(note);
        }
    }
}

fn deal(roll: &mut Roll, shared: &Shared, context: Context, request: RollRequest, grid: Grid) -> bool {
    let (ready, batch) = roll.deal(request, grid);
    if !ready.is_empty() {
        shared.add_thumbnails(ready);
    }
    let Some(batch) = batch else {
        return false;
    };
    let thumbnails = make_thumbnails(context, roll, batch, grid);
    roll.add_thumbnails(thumbnails);
    true
}

fn run_roll(shared: &Shared, kind: Kind, format: PixelFormat, decoders: &Decoders) {
    let generation = shared.board.lock().generation();
    let mut roll = Roll::new(kind, generation, list_items(Some(&shared.data), kind));
    let context = Context { data: &shared.data, format, refresh_ns: shared.refresh_ns, decoders };
    loop {
        let (request, generation, errands) = shared.board.lock().take_pass(kind);
        let Some(request) = request else {
            return;
        };
        let grid = Grid::from_request(request);
        roll.set_grid(grid);
        // What the icons stamped on the roll's items ask is done before anything else is made; the roll looks at its files again after it.
        run_errands(&roll, shared, errands);
        roll.forget_redraws(&mem::take(&mut shared.board.lock().redraws));
        if roll.is_stale(generation) {
            // The roll's files have changed, or a photo's pixel art has come or is given up: the small pictures of the items that changed are made anew.
            roll.relist(list_items(Some(&shared.data), kind), generation);
        }
        roll.set_original(request.is_original);
        let generation = roll.generation;
        let has_moved = move |board: &mut State| board.has_moved(request, generation).then_some(());
        // A round the screen has begun is told how many items the roll holds, whether an item of it stands large or not, and why it shows nothing where it holds none.
        if let Some((count, nothing)) = roll.begin_round(request.round) {
            shared.set_count(kind, count, nothing);
        }
        match roll.next_step(request) {
            RollStep::Wait => shared.board.wait(has_moved),
            RollStep::Show => {
                match kind {
                    Kind::Photo => show_pictures(shared, context, &roll.items, generation, &mut roll.cache),
                    Kind::Video => show_videos(shared, context, &roll.items),
                }
                // An item beyond the roll's end stays open with nothing to show.
                shared.board.wait(has_moved);
            }
            RollStep::Deal => {
                if !deal(&mut roll, shared, context, request, grid) {
                    shared.board.wait(has_moved);
                }
            }
        }
    }
}

fn work(shared: &Shared, format: PixelFormat) -> ! {
    let decoders = Decoders::new(Crew::on_cores("gallery", &CORES));
    loop {
        match shared.board.wait(State::take_job) {
            Job::Roll(kind) => {
                run_roll(shared, kind, format, &decoders);
                decoders.clear();
                shared.rest();
            }
            Job::Count => count(shared),
        }
    }
}

/// The main loop's handle of the gallery's thread.
pub struct Gallery {
    shared: Arc<Shared>,
}

impl Gallery {
    /// Starts the thread, idle at first, which draws in `format` and reads its files from `data`.
    pub fn start(format: PixelFormat, refresh_ns: u64, data: Data) -> io::Result<Gallery> {
        let shared = Arc::new(Shared::new(data, refresh_ns)?);
        let worker = Arc::clone(&shared);
        sys::spawn_background("gallery", move || work(&worker, format))?;
        Ok(Gallery { shared })
    }

    /// Readable when a picture, a note, a video's start or end, or the end of the thread's work waits.
    pub fn fd(&self) -> RawFd {
        self.shared.board.fd()
    }

    /// Sets the screen's request, none where no roll shows, and the pixel cam's change count the screen knows.
    pub fn want(&self, request: Option<RollRequest>, pixel_changes: u32) {
        if self.shared.board.lock().set_wanted(request, pixel_changes) {
            self.shared.board.notify();
        }
    }

    /// Adds the photos the pixel cam redrew, whose shown pictures are out of date.
    pub fn add_redraws(&self, photos: Vec<String>) {
        self.shared.board.lock().redraws.extend(photos);
    }

    /// Hands on a click on the open video.
    pub fn play(&self) {
        self.shared.board.lock().request.click();
        self.shared.board.notify();
    }

    /// Sets how the device is held.
    pub fn set_pose(&self, pose: Pose) {
        self.shared.board.lock().pose = pose;
        self.shared.board.notify();
    }

    /// Adds the errand of a stamp on a roll's item.
    pub fn add_errand(&self, errand: Errand) {
        self.shared.board.lock().add_errand(errand);
        self.shared.board.notify();
    }

    /// Sets whether home shows and the day on the screen's clock, which decide when the rolls are counted.
    pub fn set_home(&self, is_shown: bool, day: u64) {
        if self.shared.board.lock().home.set(is_shown, day) {
            self.shared.board.notify();
        }
    }

    /// True while the thread holds no file, no picture and no part of the decoder.
    pub fn is_off(&self) -> bool {
        self.shared.board.lock().is_off()
    }

    /// Swaps the newest picture into `picture` if one came for `item`, and takes the report.
    pub fn latest(&self, item: Option<(Kind, usize)>, picture: &mut Picture) -> Report {
        self.shared.board.take(|state| state.take_report(item, picture))
    }

    /// Once a refresh, swaps the due picture of the playing video into `picture`, and true if it is a new one.
    pub fn take_frame(&self, picture: &mut Picture, (flipped_ns, now_ns): (u64, u64)) -> bool {
        let mut board = self.shared.board.lock();
        let Some((_, is_new)) = board.show.take_frame(picture, self.shared.refresh_ns, (flipped_ns, now_ns)) else {
            return false;
        };
        if is_new {
            self.shared.board.notify();
        }
        is_new
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use r1_funcore::domain::gallery::{Label, Room};
    use std::thread;
    use std::time::Duration;

    pub(super) const FORMAT: PixelFormat = PixelFormat::XRGB8888;
    pub(super) const SCREEN: (usize, usize) = (480, 640);
    pub(super) const ROOM: Room = Room { screen: SCREEN, bar: 0 };
    const REFRESH: u64 = 1000;

    fn gallery() -> Gallery {
        Gallery { shared: Arc::new(Shared::new(Data::unusable(), REFRESH).unwrap()) }
    }

    pub(super) fn word_picture(word: u32) -> Picture {
        Picture { words: vec![word], width: 1 }
    }

    // The decoders with the gallery's crew, the test's own thread left on any core.
    pub(super) fn decoders() -> Decoders {
        Decoders::new(Crew::with_cores("gallery", &CORES[1..]))
    }

    #[test]
    fn thumbnails_made_on_crew() {
        let mut bytes = vec![7u8; 9];
        assert!(
            stills::read(&Data::unusable(), "photo.jpg", &mut bytes).is_err() && bytes == [7; 9],
            "without a partition nothing is read, and nothing lost of what was read before"
        );
        let decoders = decoders();
        let items = ["0.jpg", "1.jpg", "2.jpg", "3.jpg", "4.jpg"].map(Item::from_file).to_vec();
        let roll = Roll::new(Kind::Photo, 0, items);
        let context = Context { data: &Data::unusable(), format: FORMAT, refresh_ns: REFRESH, decoders: &decoders };
        let thumbnails =
            make_thumbnails(context, &roll, Batch::Photos(vec![4, 2]), Grid { places: 6, size: (228, 173) });
        let labels: Vec<(usize, Label, usize)> = thumbnails
            .iter()
            .map(|(item, thumbnail)| (*item, thumbnail.label, thumbnail.picture.words.len()))
            .collect();
        assert_eq!(
            labels,
            [(4, Label::Unreadable, 0), (2, Label::Unreadable, 0)],
            "each item gets its small picture, here the word that it cannot be read"
        );
    }

    #[test]
    fn unreadable_item_shown_empty() {
        let gallery = gallery();
        let mut picture = Picture { words: vec![8; 6], width: 2 };
        gallery.shared.show_unreadable(Kind::Photo, (3, "3.jpg"));
        let report = gallery.latest(Some((Kind::Photo, 3)), &mut picture);
        assert_eq!(
            (report.picture, report.note, picture.words.len()),
            (Some((3, "3.jpg".into())), Some(Note::Unreadable), 0),
            "an item that cannot be shown shows nothing and says so, of the file it is"
        );
    }

    fn grid(kind: Kind) -> RollRequest {
        RollRequest {
            kind,
            page: 0,
            round: 1,
            open_item: None,
            turn: 0,
            is_original: false,
            places: 6,
            place_size: (228, 173),
            margin: 0,
            room: ROOM,
        }
    }

    #[test]
    fn thumbnails_taken_once_dropped_at_rest() {
        let gallery = gallery();
        gallery.shared.set_count(Kind::Photo, 9, None);
        let unreadable = |item: usize| RoundThumbnail { round: 3, item, thumbnail: Thumbnail::unreadable("a.jpg") };
        gallery.shared.add_thumbnails(vec![unreadable(4)]);
        let report = gallery.latest(None, &mut Picture::default());
        assert_eq!(report.item_count, Some((Kind::Photo, 9)));
        let thumbnails = report.thumbnails;
        assert!(
            thumbnails.len() == 1
                && thumbnails[0].item == 4
                && gallery.latest(None, &mut Picture::default()).thumbnails.is_empty(),
            "taken once"
        );
        gallery.shared.set_count(Kind::Video, 2, None);
        gallery.shared.add_thumbnails(vec![unreadable(5)]);
        gallery.shared.rest();
        let report = gallery.latest(None, &mut Picture::default());
        assert!(report.thumbnails.is_empty() && report.item_count.is_none());
    }

    #[test]
    fn empty_roll_notes_and_rests() {
        let gallery = gallery();
        assert!(gallery.is_off());
        let worker = Arc::clone(&gallery.shared);
        thread::Builder::new().spawn(move || work(&worker, FORMAT)).unwrap();
        let next_note = |gallery: &Gallery| {
            let (mut note, mut count) = (None, None);
            while note.is_none() || count.is_none() {
                let report = gallery.latest(None, &mut Picture::default());
                (note, count) = (note.or(report.note), count.or(report.item_count));
                thread::yield_now();
            }
            (note, count)
        };
        gallery.want(Some(grid(Kind::Video)), 0);
        assert_eq!(next_note(&gallery), (Some(Note::Empty(Kind::Video)), Some((Kind::Video, 0))));
        assert!(!gallery.is_off(), "the thread stays with the roll while it shows");
        gallery.want(Some(RollRequest { round: 2, ..grid(Kind::Video) }), 0);
        assert_eq!(next_note(&gallery).1, Some((Kind::Video, 0)), "a grid that has begun another round is told again");
        gallery.want(Some(RollRequest { round: 2, ..grid(Kind::Photo) }), 0);
        assert_eq!(next_note(&gallery).1, Some((Kind::Photo, 0)), "and so is the grid of the other roll");
        gallery.want(None, 0);
        while !gallery.is_off() {
            thread::yield_now();
        }
        let report = gallery.latest(Some((Kind::Video, 0)), &mut Picture::default());
        assert!(!report.is_playing && report.thumbnails.is_empty());
    }

    #[test]
    fn busy_from_request_to_rest() {
        let gallery = gallery();
        gallery.want(Some(grid(Kind::Photo)), 0);
        let job = gallery.shared.board.wait(State::take_job);
        assert_eq!((job, gallery.is_off()), (Job::Roll(Kind::Photo), false));
        gallery.shared.board.clear();
        gallery.shared.rest();
        assert!(gallery.is_off());
        let mut epoll = sys::Epoll::with_fd(gallery.fd());
        assert_eq!(epoll.wait(Some(Duration::ZERO)).unwrap(), [7], "the main loop is woken to see it");
    }

    #[test]
    fn playing_video_reaches_screen_and_sound() {
        let gallery = gallery();
        let mut picture = word_picture(99);
        assert!(!gallery.take_frame(&mut picture, (0, 0)), "nothing plays yet");
        gallery.shared.play();
        assert!(gallery.latest(Some((Kind::Video, 0)), &mut Picture::default()).is_playing);
        let count = gallery.shared.board.lock().show.missing_count();
        for number in 0..count as u32 {
            gallery.shared.board.lock().show.add(word_picture(number), u64::from(number) * REFRESH);
        }
        assert!(gallery.take_frame(&mut picture, (0, REFRESH)) && picture.words == [0]);
        assert!(!gallery.take_frame(&mut picture, (0, REFRESH + 1)), "once a refresh");
        let ask = gallery.shared.board.lock().show.last_ask();
        assert_eq!(ask.map(|(position, _)| position), Some(0), "the sound goes by where the clock stood");
        let blank = gallery.shared.board.lock().show.take_blank();
        assert_eq!(blank.words, [99], "what the screen has done with comes back to be drawn over");
        assert_eq!(gallery.shared.hold((0, "0.mp4")).shown_count, 1);
        assert!(
            gallery.shared.board.lock().show.last_ask().is_none()
                && !gallery.latest(None, &mut Picture::default()).is_playing
        );
    }

    #[test]
    fn hold_keeps_newest_picture() {
        let gallery = gallery();
        let mut picture = word_picture(99);
        gallery.shared.show(&mut word_picture(50), Kind::Video, (2, "2.mp4"));
        let shown = |report: Report| report.picture.map(|shown| shown.0);
        assert_eq!(shown(gallery.latest(Some((Kind::Video, 2)), &mut picture)), Some(2));
        gallery.shared.play();
        let count = gallery.shared.board.lock().show.missing_count();
        for number in 0..count as u32 {
            gallery.shared.board.lock().show.add(word_picture(number), u64::from(number) * REFRESH);
        }
        assert!(gallery.take_frame(&mut picture, (0, 0)));
        gallery.shared.hold((2, "2.mp4"));
        assert!(!gallery.take_frame(&mut picture, (0, REFRESH)), "nothing plays any more");
        let report = gallery.latest(Some((Kind::Video, 2)), &mut picture);
        assert_eq!(
            (report.is_playing, shown(report), &picture.words[..]),
            (false, Some(2), &[count as u32 - 1][..]),
            "the last picture drawn, which is the one the thread keeps for a turn of the device"
        );
        let (pending, spare) = gallery.shared.board.lock().show.clear();
        assert_eq!(
            (pending.len(), spare.len()),
            (0, count),
            "every other picture is there to be drawn over, and the one the screen gave back for the one that stands"
        );
        gallery.shared.board.lock().show.add(word_picture(1), 0);
        gallery.shared.clear();
        assert!(gallery.shared.board.lock().show.take_blank().words.is_empty(), "a video let go of holds no picture");
    }
}
