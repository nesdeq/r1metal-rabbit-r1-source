use super::Shared;
use super::frames::Frames;
use crate::sync::Board;
use crate::sys;
use r1_funcore::base::pixel::PixelFormat;
use r1_funcore::codec::qr;
use r1_funcore::domain::camera::{Code, ScanStep, State};
use std::io;
use std::sync::Arc;

pub(super) fn offer(board: &Board<State<Frames>>, picture: &[u32], side: usize, format: PixelFormat) {
    if board.try_lock().is_some_and(|mut state| state.scan.offer(picture, side, format)) {
        board.notify();
    }
}

pub(super) fn start(shared: Arc<Shared>) -> io::Result<()> {
    sys::spawn_background("scan", move || work(&shared.board))
}

fn work(board: &Board<State<Frames>>) {
    let (mut picture, mut luma, mut scanner) = (Vec::new(), Vec::new(), qr::Scanner::default());
    loop {
        match board.wait(|state| state.next_scan(&mut picture)) {
            ScanStep::Read(side, format) => {
                qr::to_luma(&picture, format, &mut luma);
                if let Some(code) = scanner.scan(&luma, side) {
                    let previous = board.lock().report.code.replace(Code(code));
                    board.raise();
                    drop(previous);
                }
            }
            ScanStep::Rest => (picture, luma, scanner) = (Vec::new(), Vec::new(), qr::Scanner::default()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::Data;
    use crate::sys::Epoll;
    use r1_funcore::domain::camera::Viewfinder;
    use r1_funcore::domain::gallery::Kind;
    use r1_funcore::hw::camera::swivel::Facing;
    use std::thread;
    use std::time::Duration;

    const SCANS: Viewfinder =
        Viewfinder { kind: Kind::Photo, facing: Facing::Rear, is_scanning: true, is_calling: false, size: (464, 464) };

    #[test]
    fn code_read_once_handed_over() {
        let shared = Arc::new(Shared::new(Data::unusable()).unwrap());
        let board = &shared.board;
        let small = vec![0u32; 4];
        offer(board, &small, 2, PixelFormat::XRGB8888);
        assert!(!board.lock().scan.has_picture(), "nothing is handed over before the scanner waits");
        board.lock().set_wanted(Some(SCANS));
        start(Arc::clone(&shared)).unwrap();
        let symbol = qr::make(b"sk-proj-Fake0Key_-9", qr::Level::Medium).unwrap();
        // The code drawn four pixels a module, dark on light, with its quiet zone.
        let (pixels, quiet) = (4, qr::QUIET_ZONE);
        let side = (symbol.side() + 2 * quiet) * pixels;
        let picture: Vec<u32> = (0..side * side)
            .map(|at| {
                let module = |at: usize| (at / pixels).checked_sub(quiet).filter(|at| *at < symbol.side());
                match (module(at % side), module(at / side)) {
                    (Some(x), Some(y)) if symbol.is_dark(x, y) => 0x0000_0000,
                    _ => 0x00FF_FFFF,
                }
            })
            .collect();
        let mut epoll = Epoll::with_fd(board.fd());
        while !board.lock().scan.is_waiting() {
            thread::yield_now();
        }
        // An offer finds the lock taken while the scanner turns to wait, so the picture is offered until it is taken, as the camera offers every frame, and never again once its code is read.
        while board.lock().scan.is_waiting() && epoll.wait(Some(Duration::ZERO)).unwrap().is_empty() {
            offer(board, &picture, side, PixelFormat::XRGB8888);
            thread::yield_now();
        }
        assert_eq!(epoll.wait(None).unwrap(), vec![7], "the main loop is told");
        let code = board.take(|state| state.report.code.take()).map(|code| code.0);
        assert_eq!(code.as_deref(), Some(&b"sk-proj-Fake0Key_-9"[..]));
        assert_eq!(board.lock().report.code.take(), None, "once");
    }
}
