use crate::log;
use crate::media::still;
use crate::net::web;
use crate::openai::Account;
use crate::sys::{self, lock};
use crate::voiced::{Part, Shared, Worker};
use crate::wifi::radio::{Hold, NetworkDown, Online};
use r1_funcore::base::pixel::PixelFormat;
use r1_funcore::codec::http::{GET, POST, Target};
use r1_funcore::domain::pokedex::names::Names;
use r1_funcore::domain::pokedex::search::{Search, Step};
use r1_funcore::domain::pokedex::state::{Article, Lookup};
use r1_funcore::domain::pokedex::{self, ADDRESS, ArtworkFault, HEADERS, Language, QUERY_HEADERS};
use r1_funcore::domain::voice::{Phase, Query};
use r1_funcore::gfx::Picture;
use r1_funcore::gfx::palette::Roles;
use r1_funcore::net::fetch::check_plain;
use r1_funcore::net::tls::Purpose;
use r1_funcore::openai::fault::Fault;
use r1_funcore::openai::transcript::Subject;
use std::ffi::CStr;
use std::io;
use std::sync::Mutex;
use std::time::Instant;

/// The Pokédex job's worker, with the names of the language last asked and how an artwork is drawn.
pub struct Searcher {
    names: Mutex<Option<Names>>,
    format: PixelFormat,
    side: usize,
    online: Online,
}

/// The main loop's handle of the Pokédex job.
pub type Pokedex = Part<Searcher>;

impl Pokedex {
    /// Starts the threads, idle until the screen shows, which draw an artwork in `format` in a square of `side` pixels.
    pub fn start(account: Account, online: Online, format: PixelFormat, side: usize) -> io::Result<Pokedex> {
        Part::spawn(Searcher { names: Mutex::new(None), format, side, online }, false, account)
    }

    /// Sets the colours an artwork is drawn on.
    pub fn set_roles(&self, roles: Roles) {
        self.shared.board.lock().job.roles = roles;
    }

    /// Sets the language names are said in and entries written in.
    pub fn set_language(&self, language: Language) {
        self.shared.board.lock().job.language = language;
    }
}

// Sends one request of `method` with its headers and body on a connection of its own, and reads its answer's body under the limits of a fetched document.
fn fetch(
    address: &str,
    (method, headers, body): (&str, &[(&str, &str)], &[u8]),
    down: &NetworkDown,
) -> io::Result<Vec<u8>> {
    let target = Target::parse(address).ok_or_else(|| sys::invalid(pokedex::to_no_url_text(address)))?;
    let start = Instant::now();
    web::ask_once(web::reach(&target, Purpose::Http, down)?, (method, &target), (headers, body), |answer| {
        check_plain(&answer.head).map_err(io::Error::other)?;
        answer.read_document(start)
    })
}

fn post(body: &str, down: &NetworkDown) -> io::Result<String> {
    Ok(String::from_utf8_lossy(&fetch(ADDRESS, (POST, &QUERY_HEADERS, body.as_bytes()), down)?).into_owned())
}

impl Worker for Searcher {
    type State = Lookup;
    type Stay = Hold;
    const NAME: &str = "pokedex";
    const THREADS: [&str; 2] = ["pokedex", "lookup"];
    const FILE: &CStr = c"pokedex";
    const DOWN: fn(&Hold) -> &NetworkDown = Hold::down;

    fn stay(&self, shared: &Shared<Self>, needs_key: bool) -> Result<Hold, Fault> {
        if needs_key { shared.account.hold() } else { self.online.hold() }
    }

    fn turn(&self, shared: &Shared<Self>, hold: &mut Hold, queries: Vec<Query>) {
        queries.into_iter().for_each(|query| self.look_up(shared, query, hold.down()));
    }
}

impl Searcher {
    fn artwork(&self, address: &str, roles: Roles, down: &NetworkDown) -> io::Result<Picture> {
        let file = fetch(address, (GET, &HEADERS, &[]), down)?;
        let png = pokedex::read_artwork(&file).map_err(|fault| sys::invalid(fault.to_string()))?;
        png.picture(still::inflate(&png)?, roles.background, (self.side, self.side), self.format)
            .map_err(|fault| sys::invalid(ArtworkFault::Unpainted(fault).to_string()))
    }

    fn look_up(&self, shared: &Shared<Self>, query: Query, down: &NetworkDown) {
        let result = self.find(shared, query, down);
        shared.tell(|state| state.take_lookup(result));
    }

    fn find(&self, shared: &Shared<Self>, query: Query, down: &NetworkDown) -> Result<Option<Article>, Fault> {
        let language = shared.board.lock().job.language;
        let prompt = pokedex::prompt(language);
        let subject = (Subject::PokedexName, prompt.as_str());
        let said = shared.write_down(query, down, subject, "pokedex: the name said is not written down", None)?;
        shared.set_phase(Phase::Searching);
        self.search(&said, language, || shared.board.lock().job.roles, down)
    }

    // A lookup of what was said, each of its steps as funcore's `Search` says it; an artwork is drawn on the colours `roles` gives then.
    fn search(
        &self,
        said: &str,
        language: Language,
        roles: impl Fn() -> Roles,
        down: &NetworkDown,
    ) -> Result<Option<Article>, Fault> {
        let mut search = Search::new(said, language, lock(&self.names).as_ref());
        loop {
            let lines = match search.step() {
                Step::Names(body) => {
                    let answer = post(&body, down).map_err(|e| e.to_string());
                    search.take_names(answer, &mut lock(&self.names))
                }
                Step::Entry(body) => search.take_entry(post(&body, down).map_err(|e| e.to_string())),
                Step::Artwork(address) => {
                    let picture = self.artwork(address, roles(), down).map_err(|e| e.to_string());
                    search.take_artwork(picture)
                }
                Step::Done => return search.finish(),
            };
            lines.iter().for_each(|line| log!("{line}"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::Data;
    use crate::sys::Epoll;
    use crate::wifi::radio::tests::{up, without_networks};
    use r1_funcore::domain::cause::Cause;
    use r1_funcore::domain::voice::Bid;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn typed_name_no_network_fails() {
        let radio = without_networks();
        let account = Account::new(Data::unusable(), radio.online());
        let pokedex = Pokedex::start(account, radio.online(), PixelFormat::XRGB8888, 287).unwrap();
        let mut epoll = Epoll::with_fd(pokedex.fd());
        assert!(pokedex.is_off());
        pokedex.ask(vec![Bid::Text("pikachu".into())]);
        while !pokedex.is_off() {
            thread::yield_now();
        }
        assert_eq!(pokedex.latest().phase, Phase::Idle, "with the screen closed nothing is looked up");
        pokedex.want(true);
        pokedex.ask(vec![Bid::Text("pikachu".into())]);
        let mut phase = Phase::Idle;
        while phase == Phase::Idle {
            assert_eq!(epoll.wait(Some(Duration::from_secs(10))).unwrap(), vec![7], "the screen is told");
            phase = pokedex.latest().phase;
        }
        while !pokedex.is_off() {
            thread::yield_now();
        }
        assert_eq!(
            pokedex.latest().phase,
            Phase::Failed(Cause::NoNetwork),
            "here without the driver's nodes the radio does not come on, and a name typed needs no key"
        );
        pokedex.want(false);
        assert!(pokedex.is_off());
    }

    #[test]
    #[ignore]
    fn live_site_answers() {
        let down = up();
        let online = without_networks().online();
        let searcher = Searcher { names: Mutex::new(None), format: PixelFormat::XRGB8888, side: 287, online };
        let found = searcher.search("Glurack", Language::German, Roles::default, &down).unwrap();
        assert!(
            lock(&searcher.names).as_ref().is_some_and(|names| names.count() > 2000),
            "every Pokémon's German and English names"
        );
        let Article { entry, picture } = found.expect("the Pokémon whose name is nearest");
        let picture = picture.expect("its artwork");
        println!("{entry:?}\n{} by {} pixels of artwork", picture.width, picture.words.len() / picture.width);
        assert_eq!((entry.number, picture.width), (6, 287));
    }
}
