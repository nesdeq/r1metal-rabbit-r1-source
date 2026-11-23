# Style guide

Rules for every screen. A screen that breaks one is a bug. `examples/shot.rs` renders the screens its `SCREENS` names in every theme (`shot themes`) and films one screen with its other verbs; the passphrase's window has no render, and the debug screen one in Stage alone. Check a new screen, icon or animation against this page in its renders, then judge it on the device. What each screen does is in [Interface](interface.md) and [Camera](camera.md).

## 1. Priorities

| Rule | Enforced by |
|---|---|
| Anything that moves is drawn at every refresh (60 Hz). A missed refresh is a bug. | the log line for a run with late frames; at the logging level `debug` the frame records ([Display](../firmware/display.md#timing)) |
| Only heavy frames, and user input or a chat command with the frame that shows its result, get the full CPU clock, and only while they last. | `Ui::is_straining`, `sys::set_clock_floor`, `r1_funcore::domain::app::Boost` ([Display](../firmware/display.md#the-drawing-thread)) |
| Logging, sensor readings, file writes and snapshots never take a core from the drawing thread. | The drawing and camera threads, the `calls` thread and a call's microphone and speaker run real time; every other thread runs ordinary or lower, and each new thread states which ([System](../firmware/system.md#the-app)). |
| Drawing a frame allocates no memory. | room for a frame's strips and canvases is made with the interface (`DisplayList::new`, `Ui::make_canvas_room`); allocation-counting tests below the interface (`base/alloc_count.rs`) |
| Motion comes in short bursts, then rests. An unchanged screen is not redrawn, nor a line whose cached picture is current. | `Ui::is_animating`, `Ui::does_redraw`; `deal_lines`, `Item::to_key`; the cursor blinks only where shown (`Ui::does_caret_show`) |
| A screen switches off what it switched on (camera, decoder, accelerometer, radio) when it closes and before sleep. | The main loop suspends only when every part reports idle; what runs asleep is in the alarm's list ([Sleep and clock](../firmware/sleep.md)). The battery rundown runs until switched off; sleep switches it off. |
| Screens are judged on the device with real content. Host renders (`shot`) prove nothing. | |

## 2. The screen

The panel is 480x640. The CPU draws every pixel in the three colours of one palette on black, laid out by one of six themes (see 12). Icons are Google's Material Symbols Rounded, plus the older Material Icons Round where Symbols has no match. Text uses Spleen, a monospaced bitmap font: 16x32 for titles, names and values (heading font, `Ruler.heading`), 12x24 for everything else (body font, `Ruler.body`) ([Credits](../credits.md)). A character Spleen lacks is GNU Unifont's, in the colour of the writing around it and grown to its height with strokes as strong as Spleen's, one cell wide or two. An emoji is Noto Emoji's picture in its own colours, the one thing on a screen that is not of the palette beside a photo: two cells wide, 24x24 in the body font and 32x32 in the heading font ([Display](../firmware/display.md#shapes-and-text)). Terminal writes everything in 12x24, as type is one size (`Look.is_one_size`, `Ruler::set_look`); the gaps stay those of the other themes.

## 3. The bar

On every screen except home, the top 97 px are the bar. The robot (114x81) sits at its right end, 8 px from the top and right edges; nothing else is drawn under it. Everything else goes in the area under the bar (`Ruler::room`). On a video shown large the bar runs along the edge on top for the holder; in a viewfinder the robot turns in place (58x81). The key window has its own bar with the robot at its right end (see 7).

Home is where the robot sits large (see 12). In Stage the main stage opens on the robot, 372 px wide and centred, in the slot of the chat icon, the stage's first. The button squeezes it and the wheel pulls it like any icon, and it stays upright for the holder (see 7). When the wheel moves on, it flies to the bar; a swipe down from the top brings it back. In the menu themes it sits at home in the square it turns in, and flies to the bar when a screen or menu opens; in Rail it stays in the bar at home too.

- **Title and bar subtitle** (`BarWords` in `ui/screen/bar.rs`). The screen's name is at the left in the heading font, with one line under it, the bar subtitle, showing the first that applies: what the screen is doing or why it failed (see 11); the primary figure, in themes that show it in the bar; the selected item's position (`3 of 14`, not counting an action line); what the screen is about.
- Stage, Dial, Deck and Rail dim the bar subtitle. In Rail the bar stands in the pane right of the rail, the robot at its right end; beside a line that opens no screen in the pane it names that line, the level whose lines the pane shows, over what that line tells. Terminal shows the path in lowercase (`home`, `settings`) over a rule, plus a status line at the bottom of the screen whose left end shows `--More--` in place of what the screen tells while more of what shows follows below, over a text with how much of it has shown (`--More--(28%)`) (see 8), and whose right end shows a state (`VOICE`, `MUTED`, `KEYS`, `MAGIC ON`, `IN USE C`), otherwise the battery level followed by `CABLE` on a cable and `WIFI`, or `MOBILE` over the modem, in a network, then `ALARM` or `RINGING` and the time the timer has left. Companion shows a sentence spoken by the robot in the content colour (`All is set. WiFi: Upstairs.`), or the selected item's position on a screen without one.
- Chat's title is the robot's name (`ROBOT` until it has one).
- **Progress row.** Between the title and the robot, a row of faint blocks lights one by one to show a slow job's progress, and disappears when done. Only the pixel cam uses it.
- Nothing else goes in the bar, and no screen draws into it.

## 4. Colours

A palette has three colours on black, one per kind of content. `COLORS` offers sixteen. Each theme keeps its own palette; the current theme's palette colours every screen and every PDF written while it is set.

| Colour | Used for | Never for |
|---|---|---|
| Interface (`Palette::interface`) | names, labels, icons, the clock, lit blocks, the selection frame; a PDF page's header bar and footer line | text-screen text |
| Content (`Palette::content`) | text-screen text and its PDFs, the translator's transcript, chat messages, the robot's sentences in Companion | anything else |
| Mark (`Palette::mark`) | markup: headings, list markers, rules, bold and italic; the robot everywhere; the icons of Companion's lines | anything else |

| Palette | Interface | Content | Mark |
|---|---|---|---|
| A, sand | `D4B997` 11.2 | `F2E8D7` 17.3 | `C07851` 6.0 |
| B, slate | `AFBFD5` 11.2 | `E4EAF1` 17.3 | `6D9DCE` 7.4 |
| C, amber (standard) | `FFE47A` 16.6 | `FFB000` 11.5 | `F06A00` 6.8 |
| D, graphite | `BEBEBE` 11.3 | `E8E8E8` 17.1 | `FCA942` 10.9 |
| E, sunset | `F8B478` 11.8 | `F5EAD9` 17.7 | `AE7EE2` 6.9 |
| F, nixie | `FF7A2E` 8.1 | `FFD0B0` 14.9 | `C25B20` 4.8 |
| G, coral | `FED8C7` 15.9 | `FEB0A6` 12.0 | `D66778` 6.1 |
| H, rose | `F586A7` 8.9 | `FEDCE1` 16.5 | `C75783` 5.1 |
| I, orchid | `FCD7F8` 16.2 | `ECAFEF` 11.9 | `BF6DCC` 6.3 |
| J, iris | `B696F7` 8.7 | `EAE1FE` 16.7 | `8E68DC` 5.2 |
| K, ice | `6BB9F8` 9.9 | `D8EDFC` 17.4 | `4087DE` 5.7 |
| L, arctic | `C2F2FA` 17.4 | `7ADFF7` 13.7 | `26A7D0` 7.5 |
| M, lagoon | `52CEC4` 11.0 | `D0F2EF` 17.6 | `149D99` 6.3 |
| N, aurora | `8CE6C4` 14.2 | `D8EEF4` 17.5 | `AB8BE3` 7.5 |
| O, phosphor | `B8FACE` 17.6 | `72F08B` 14.6 | `08BA87` 8.4 |
| P, moss | `B4C786` 11.5 | `ECEBD1` 17.4 | `CE9B2B` 8.3 |

- Numbers are contrast ratios against black. The neutrals and the standard come first, then the palettes go round the colour wheel from orange.
- In each palette the interface and content are light steps of one hue, one of them pale and one more saturated, and the mark is its deepest, most saturated step: emphasis by colour, never by glare. Sand, slate and graphite are neutral, sunset and aurora two families of light, and each is marked by a colour of its own.
- Every palette meets the guidelines for readable pages: what steps back of the interface at least 3:1, what a text marks and content's dim step at least 4.5:1, content at least 7:1.
- **Dim step** (`text_dim`, `content_dim`): the colour divided by φ (in C `9E8D4B`, `9E6D00`), for the bar subtitle and line subtitles, unselected frames, lines that can do nothing now, the user's chat messages, and a translator remark as spoken above its translation.
- **Faint step** (`rule`): the interface colour divided by φ² (in C `61572F`), for unlit blocks, rules and the text cursor. Cards are filled with a much weaker interface colour still (`Roles::surface`).
- **Signals**, the same in every palette: green `559A00` (`saved`) for a battery held at its level on a cable; red `FF4300` (`alert`) for what needs attention while it lasts: a battery below one bar with nothing charging it, a recording's mark, an armed stamped icon, the bell while it rings.
- The selection has no colour of its own: a frame, reverse video in Terminal, the large character over the touched key, or on the clock and volume screens everything else dimmed, in Dial the band.
- **Single source**: `r1_funcore::gfx::palette`. A `Palette` holds the three colours; `PALETTES` lists the sixteen; `STANDARD` is a new device's, C; `PALETTE_NAMES` in `ui/words.rs` names them. `Roles::of` derives `background`, `text`, `text_dim`, `rule`, `glow`, `robot`, `content`, `content_dim`, `mark`, `saved`, `alert` and the cards' `surface`. Screens use their theme's roles; a summary or rundown PDF uses the palette set when it is written.

## 5. Sizes

Related sizes differ by powers of φ (the golden ratio) from the 50 px height of the clock digits:

| Size | Used for |
|---|---|
| 212 px | the icon in the middle of a stage |
| 81 px | a docked icon; the robot in the bar |
| 31 px | a stamped icon |
| 19x12 px | a block (brightness, volume, signal, progress row) |
| 7x12 px | a clock tick |
| 5 px, 3 px | the selection frame; the outline of each grid cell |
| 8, 13, 21 px | gaps; 8 px is the standard gap |

- **Icons** are Material files as drawn, only scaled: the file's whole box is drawn at the icon's box. The sizes above are the live area, 5/6 of the box (`LIVE_SHARE`), where Material's keylines sit; a few icons (wifi among them) extend past it. Box sizes: 254 px in the middle of a stage, 97 px docked, 37 px stamped, 48 px on a key, 60 px for the battery (one scale for all battery icons, so it does not jump).
- Symbols (grid of 960) are in `crates/funcore/assets/material`, fetched by `scripts/fetchicons.sh`; the older Material Icons Round (grid of 24) are in `material-icons` and used only where Symbols has no match (`icon!(older NAME)`): only `catching_pokemon`, the Pokédex. Icons are used by file name only.
- On a card an icon is the golden ratio times as tall as the largest writing under it and stands at the left on that writing, whatever the card's size: 52 px on a tile, over its name; 104 px over a value written twice as large; smaller only where a card has no room over its writing (`Ruler::card_icon`).
- The robot at home is its drawing in a box φ times an icon box, 372x265 px, keeping the drawing's line widths. Its switch's icon is the drawing in an icon box.
- Like things have one size: grid cells, list lines, keys of one row, every name and every value of every list.
- Every interface number is a named constant; a design choice is marked as such on this page until judged on the device.

## 6. Motion

- Anything that changes position, size or content animates there. A cut is a bug, except in Terminal, which prints and cuts.
- Every moving edge is a damped spring stepped every 1 ms (`Edge`, `Spring` in `r1_funcore::gfx::motion`), listed below by undamped rate (rad/s) and damping ratio (1 does not overshoot). An edge is at rest within 0.5 px of its target and under 5 px/s; a share (0 to 1) within 1/256.
- Anything that moves more than its own height per frame looks like a low frame rate, so small items get springs whose per-frame step stays well under their height.
- Every animation ends at rest, and at rest nothing is drawn.
- The icon stage defines the moves: the leading edge is pulled while the trailing edge holds, then released and flown off; an arriving item overshoots slightly; the button squeezes; at an end an item swings back, which is also how it refuses; rows arrive one after another; a new picture unfolds from its centre; a frame moves leading edge first.
- What is put anew where something showed unfolds from its centre over what showed there (`gfx::store`): a picture in a grid's place; a room kept under the bar drawn anew while it shows (a text that changes while it is read, the browser's next page, the Pokédex's next result, the conversation in another theme or palette, the translator's next session); the line in Dial's band painted anew where it stands; the bar subtitle. A word that comes while another unfolds opens in its place. What changes in place stays as it is: rows written as they come (a reply growing, a remark heard), a text the wheel scrolls, a line that rolls into Dial's band, a setting in that band as the wheel sets it, and a bar subtitle that differs only in its figures (`2 of 7`, `LISTENING 0:04`), which counts on as a clock does. Terminal cuts every one in (`Ui::does_cut`). Tests: `gfx/store.rs`.

Screen transitions depend on the theme (`Look.motion`, `Motion` in `r1_funcore::ui::layout`). Screens slide on `SLIDING`; printing and typing advance once per refresh (`REFRESH_NS`).

| Theme | Transition |
|---|---|
| Stage, Companion | the new screen follows the old one out from the opposite side, forward from below, back from above; Companion then types the bar's sentence a letter per refresh |
| Dial | the old screen rolls over the edge as on a drum; the new one rolls in after it |
| Deck | the new screen is dealt from below as a card over the old one; going back, the top card is taken off downward |
| Terminal | cut, then the new screen prints from the top, one heading-font line per refresh |
| Rail | as Stage, the pane alone, for a branch in the pane's lines; everything else in place: what the pane shows of the line the rail rests on comes in place, lines that already show take the wheel in place, a level's lines move between the pane and the rail in place, its rows arriving on the rail one after another |

| Spring | Rate, damping | Moves |
|---|---|---|
| `SWING` (`gfx/motion.rs`) | 28, 0.7 | anything that settles with a slight overshoot: a picture or the bar subtitle unfolding, an item squeezed or swung against an end, an item landing large, a key's large character, a key layer dropping in, typed text refusing, brightness and volume blocks at an end |
| `PULL` (`gfx/motion.rs`) | 40, 1 | the edge the wheel pulls: a stage icon, an item shown large |
| `HOLD` (`gfx/motion.rs`) | 30, 0.6 | the edge that holds meanwhile |
| `EXIT` (`gfx/motion.rs`) | 14, 1 | the front edge of a released item |
| `FREE` (`ui/list/stage.rs`) | 28, 0.6 | an icon edge settling: arriving, docking, returning |
| `TRAILING` (`ui/list/stage.rs`) | 50, 0.3 | an icon's rear edge, one box behind the front, so the icon stretches and squashes |
| `FOLLOWING` (`ui/picture/carousel.rs`) | 160, 1 | a large item's rear edge, close behind the front, so a photo lands without wobble |
| `PAGING` (`ui/grid.rs`) | 36, 0.8 | the selection frame's leading edges; rows arriving or leaving with a page |
| `TRAILING` (`ui/grid.rs`) | 28, 1 | the selection frame's rear edges, so it stretches in transit |
| `OPENING` (`ui/grid.rs`) | 30, 1 | a roll item growing out of its cell or shrinking back |
| `SEATING` (`ui/view/settle.rs`) | 26, 0.65 | the robot between home and the bar; the clock's marker on its scale |
| `BLOCK` (`ui/view/settle.rs`) | 30, 0.5 | blocks changing size: brightness, volume, progress row, microphone meters |
| `SLIDING` (`ui/view/settle.rs`) | 30, 1 | slides without overshoot: screen transitions, chat's keyboard and typed text on mute, Dial and the drum rolling to the selection, a viewfinder squeezed, a note sliding in, the key window rising and falling |
| `TURNING` (`ui/view/settle.rs`) | as `SLIDING` | the robot turning upright at home; it cuts a hand's 8 to 12 Hz tremor to a quarter after smoothing has cut it to a fifth |
| `SCROLLING` (`ui/view/settle.rs`) | 12, 1 | text the wheel scrolls (text screen, chat, translator); at one step per 300 ms, a reading hand's pace, it never stops between steps, moves at most a third of a line per frame and never overshoots |
| `RISING` (`ui/keys/keyboard.rs`) | 16, 0.8 | an opening keyboard's rows: under half a key height (78 px) per frame, slight overshoot |
| `FOLDING`, `TRAVELLING` (`ui/keys/keyboard.rs`) | 40, 1; 60, 1 | the large character folding into its key, and moving to the key the finger slid to |
| `CARET` (`ui/keys/keyboard.rs`) | 40, 0.8 | the text cursor along its line; it jumps between lines |

**Spring inputs.**

- The wheel pulls an icon's front edge by 0.6 of its box (`PULL_SHARE`). At 0.9 of that (`RELEASE_SHARE`) it releases the icon, whose front edge heads 1.5 boxes past the screen edge (`EXIT_BOXES`); the holding edge gives way by 1/8 of the pull (`GIVE`).
- After a release the rest of a spin is ignored until the wheel pauses 150 ms and the next icon has landed (`PAUSE_NS`). A step back before the release cancels the pull; a step the other way always counts, at once if the new icon has landed.
- The button squeezes each icon edge in by 1/12 of its box (`PRESS_SHARE`), and a grid's selected cell by one gap.
- An item shown large is pulled 81 px until the next picture arrives.
- An item pushed against an end starts at 480 px/s (`NUDGE_SPEED`), about one gap out and back.
- Rows start 25 ms apart (`STAGGER_MS`).
- A new key layer drops in from 1/4 of a key height (`DROP_SHARE`).
- Held delete repeats after 400 ms, then every 100 ms (`HOLD_NS`, `REPEAT_NS`); a character held 400 ms offers its other forms.

**Timed changes.**

- The text cursor is lit 618 ms (1 s / φ, `CARET_NS`) and dark as long, counted from the last input.
- Terminal prints a line per refresh, Companion types a letter per refresh, and theme cards render one per refresh after the selected one.
- A note about one photo, recording or request shows for 1 s (`NOTE_NS`).
- A field too long for its space (see 8) rests at its start for 2.618 s (1 s × φ², `TICK_REST_NS`), scrolls to its end in 1.618 s (1 s × φ, `TICK_TRAVEL_NS`) with easing, rests, scrolls back, and repeats. All visible fields move together and restart when the screen, the stage or menu in front, or the selection changes. Nothing is drawn for them at rest. Both times are design choices.

## 7. Layout

- Nothing overlaps; if two things need one place, one moves.
- The windows that ask for what the device cannot go on without, the safe's passphrase and the OpenAI key, the key's window where it scans a tunnel's config instead, which may always be left ([Mobile data](interface.md#mobile-data)), the bell's window, which tells what rings, and the window of a call that rings, which names whom it is from, take the whole screen and rise from below over the screen they stand over, which shows only while they rise or fall and takes no input (`ui/view/window.rs`). Each is laid out like a screen: its bar with the robot at the right end; the passphrase window its dots under the bar and the keys every keyboard has at the foot, as the password's keyboard, the key window, the bell's and a call's their lines as the theme lays out a list ([Interface](interface.md#the-passphrase), [The bell](interface.md#the-bell), [Calls](interface.md#calls)).
- A secret that is typed, a password, the SIM's PIN or the passphrase, shows a dot a character in the large writing until the eye at the right end of its line, in the dim interface colour, shows it; struck through, the eye hides it again (`EYE_ICONS`, `draw_secret` in `ui/edit.rs`). Every secret begins hidden.
- Nothing is drawn over a picture; text goes in the bar. Exceptions: a recording's mark; camera messages and the roll's position (`SAVED`, `2/7`) at the bottom; a roll's stamped icons (31 px live area in the interface colour, a 3 px dark rim, one gap in from the foot: delete at the right corner, and on photos only new pixel art `frame_reload` at the left corner and `send` in the middle).
- Only things with a long side turn with the device: a video shown large, and the robot and text in a viewfinder. Pictures, grids, lists and stages never turn.
- A screen of a setting (brightness, volume, clock) centres what it sets in the area under the bar, or puts it at the bottom where the theme does (`Layout::place_gauge`, `Layout::place_clock`); the volume screen stands its two rows, the volume and the interface's sounds, one under the other, the one the wheel does not set stepped back to the dim interface colour. In Dial its band holds one setting at a time, a click rolling in the next. In Deck each is one card over the whole room under the bar, as a page of one card fills it (see 8), the volume screen's two cards sharing the room, its icon over its writing as on every card (see 5), everything inset by the card inset.

**Robot tilt at home.** The robot at home is the only thing turned by the accelerometer alone: it stays upright for its holder. It turns about its seat's centre to follow gravity in the screen plane (`pose::upright_turn`), readings smoothed of tremor, on `TURNING`, the short way round (`Ui::set_tilt`).

- Within 5° of upright it stays upright; beyond 5° it follows the hand, and returns to upright only below 4°, so it does not flip at the boundary (`UPRIGHT_TOLERANCE`, `UPRIGHT_RETURN` in `ui/screen/seat.rs`).
- Within 24° of flat (under 4000 mm/s² in the screen plane, `FLAT_LIMIT`) it keeps its turn.
- A reading that would move no corner of its seat by 0.5 px is ignored (21 mm/s² across gravity when upright), so a still device draws nothing.
- It is as large turned as upright: every home keeps the square it turns in, its box's diagonal, clear of text (`diagonal` in `ui/ruler.rs`).
- A turn never interrupts a gesture, wakes nothing and is not use. Squeezed or poked, it stays turned.
- When the wheel moves it off home it turns back to the nearest upright on its way to the bar; back home it keeps the screen's orientation until a reading arrives. In the bar it turns only with the bar.

## 8. Grids and lists

All grids and lists use one mechanism (`r1_funcore::ui::grid`): pages of equal cells in reading order, a fixed number across and down whatever the item count, overflow on the next page. Each item has a dim 3 px outline; a moving 5 px frame in the interface colour marks the selection. Nothing is centred, spanned or resized because of the item count, but on Deck's board, whose pages fill their room (below). A line that does not fit a page's room goes to the next page, and the room it leaves stays empty: no page keeps room for anything, and no line moves or grows into it. Wherever a page leaves room under its lines while lines wait on the next page, a small arrow pointing down stands in the middle of that room: Material's `keyboard_arrow_down`, its box as tall as the small writing, in the dim interface colour, shown wherever the chevron it draws fits, however little room the page leaves (`Grid::spare_rooms`, `list::draw_more`). It comes and goes with its page; a full page, a last page and Deck's pages show none, and Dial has no pages. How a theme tells that more follows is its own (`ListStyle.has_arrow`, `BarStyle.has_pager`): Stage, Dial, Deck, Companion and Rail by marks, this arrow and the text screen's scrollbar (below); Terminal as the pager `more` does, with no mark: while a page follows the one that shows, whatever room that page leaves, the left of its status line shows `--More--` in place of what the screen tells (`Grid::has_more`, `Ui::write_more`).

| Grid | Per page |
|---|---|
| Roll | 2 across, 3 down; a drum in Dial |
| List | 6 lines in Stage, Dial and Rail, 7 in Companion; in Terminal as many heading-font lines as fit |
| Deck board | 2 across, 4 down |
| Home | as many cells as fit (`Layout::home_grid`); Dial scrolls through all of home's lines |
| Rail | the deepest level on the way in that stands on the rail, 1 across, as many cells down as the panel's height holds over the rail's foot (`Ruler::rail_foot`), each a line's icon inset as text is in a bold frame (66 px wide) |

The grids of all list screens are in one table (`List::SCREENS`, `List::of`).

- A line is a name in the heading font over its dim subtitle (`Item.word`).
- Entries behind their kind's mark (Companion) fit as many per page as their minimum height allows, share the page height and are centred in their share.
- **One writing per field.** Every field has the writing its place gives it, whatever it holds: a name or a value in the heading font, a label, subtitle or what follows from a value in the body font, a value written large twice or three times the heading font where the theme writes it so. A field too long for its room scrolls sideways (see Fields below); none is ever written smaller to fit.

**Deck board.** In Deck every list sits on the board of home's tiles and the settings: 2x4 cells under the bar (`Page::Board`, `BOARD_COLUMNS`, `BOARD_ROWS`), each 228x128. A figure, an action and a menu tile take one cell; other lines and entries two side by side; a palette card two stacked (`CARD_ROWS`); the primary figure four (2x2) under its screen's icon. A page ends where the next row does not fit; a band of a different kind starts a new row.

- **A page fills its room** (`Grid::with_fill`). A page holds what the board puts on it, and its cards stretch over the whole room: a run of tiles fewer than two rows hold stands one to a row across the room (one alone takes the whole room, two take half each, three a third), where the page has the room for that; otherwise two across, the last of an odd run as wide as its row (four stand 2x2, five 2, 2, 1). Every row then grows alike to the room's foot. A full page stands as the board has it.
- **Home pins its card** (`Grid::with_pins`). The conversation's card stands on every page of Deck's home where it stands on the first, with the robot on it; the pages of tiles turn under it and fill the room below it.
- **Cards.** A menu tile, a way down and a line with an icon are tiles: the icon standing on the name (see 5), the name over its word or the beginning of its text at the foot, on the same row in every tile, a chevron at the name's right where a list's line opens a page. A figure writes its label, value and what follows at the foot too. A line without an icon is a row on a card; an entry a card of its own (below).

**Line roles** (`Role` in `r1_funcore::ui::item`): `Plain` acts in place or opens something; `Primary` is the value the screen is about (the primary figure), with its fill level where it has one; `Figure` is a label over a value; `Action` is a way down or out, with its icon; `Entry` is a saved item (creation time, length, start of its text). The theme gives each role a form, which is its shape and painter (`ListStyle.primary`, `ListStyle.forms`, `ListStyle::form_of`); a list is laid out in bands of those shapes (`Band`, `Grid::with_bands`), a new band per section. A line that opens a page ends with the theme's marker: a chevron, `›` in Terminal, none in Rail (`RowStyle.has_leads`).

**Counts.** A list's action line comes first and is not counted: `RECORD` (notes), `TRANSLATE` (translations), `LISTEN AGAIN` (networks), `NEW` (the bell, where the bar shows nothing on it while something is set, else `NONE SET`; the rundowns, where it shows nothing on it while one is kept, else `NONE KEPT`), `NEW CALL` (the calls). On an entry the bar shows `2 of 3`; on the action line the count (`3 NOTES`, `1 NOTE`, `7 NETWORKS`, `7 CALLS`) or `NONE YET`, `NO NETWORKS`, `NO CALLS YET`. Activity or a failure takes precedence. A list without an action line counts every line.

**Fields** (a name, a value, a subtitle, the bar subtitle, the status line, the label under a stage icon) show whole in their writing: where too long for their room they scroll sideways to the end and back (see 6), at any size, the figure written large too (`paint_field`, `paint_scaled`, `draw_field`), never truncated, no ellipsis. A text preview (an entry's text, a text's line on its page, the robot's last reply on Deck's chat card) shows as many whole words as its lines hold, no ellipsis; a word longer than a line breaks at the line's end.

**Behaviour.**

- A line that can do nothing now is dim and swings when clicked.
- A setting's line names the setting over its value and sets it where it stands, in a menu or in a screen's list (`Content::Setting`, `ui/screen/settings.rs`): a click turns a switch; any other a click stages, the bar saying `CLICK TO SET` (on Stage's stages, which have no bar, a line under the value), the wheel steps, swinging past either end, and the next click sets. Another line, back, home, sleep and a new theme let go of what is staged.
- An information-only list is in the interface colour; a clicked line swings.
- A list that informs and also leads elsewhere puts information first and actions last, never on the first line.
- No single press or touch loses anything, and nothing asks for confirmation: forget and delete sit one level in, on the item's page, never as its first line, and last on a note's or conversation's page. A stamped icon arms on the first touch (red) and acts on the second; `send`, which loses nothing, acts on the first. On a rundown's page a feed's line and `DELETE` arm at the first click, saying `CLICK AGAIN TO REMOVE` or `CLICK AGAIN TO DELETE`, and act at the second; the wheel or a click elsewhere disarms ([Interface](interface.md#rundowns)).
- An item's page has one line per action, and a line per text of the item (its name over its start).
- Nothing is saved until it works: a network is remembered only after a successful join, an OpenAI key only after a successful check. The bar shows the result.
- When a list's order changes it is dealt again: the old lines leave upward, the list arrives from below.

**Text screen** (`Screen::Reader`), which shows a summary, transcript, conversation or rundown: the whole area under the bar; headings in the heading font, the rest in the body font, 38 characters a line, half a line between blocks and a full line above a heading. Bold is one pixel wider and italic slanted (`Lettering`), both in the mark colour with the rest of the markup; plain text in the content colour. `Row::is_marked` in `r1_funcore::domain::paper::rows` decides markup for screen and PDF; an unmatched marker marks nothing. The bar shows the text's name over its item. The text slides up with its screen, or, without a screen transition, rises from one wheel step below. The wheel scrolls 4 lines a step (`STEP_LINES`) on `SCROLLING`, swinging one gap at either end. The button only swings it; only the left-edge swipe leaves. Every text the wheel scrolls (the text screen, chat, a Signal conversation, the browser, the Pokédex, the translator) tells alike what of it shows (`Ui::text_room`): in the themes of marks a thin dim scrollbar in the right gap shows the visible part, hidden when all fits (`Ui::draw_scroll_mark`); in Terminal the left of the status line shows `--More--` and the share of the text's height shown down to the foot of its room, `--More--(28%)`, in place of what the screen tells, until its end shows.

**Chat** (`Screen::Chat`): its days fill the area under the bar from the top, the newest at the bottom once full, in the text screen's fonts and colours: the user's messages in content dim, the robot's replies in content with markup in the mark colour, no part of a reply set apart; a web search as one dim italic line, `searched online`; cited pages not shown. The wheel scrolls it like the text screen.

**Web** (`Screen::Web`): the page's text from the top of the area under the bar, set like the text screen, each link's number in brackets behind its words, in bold; the wheel scrolls it. While a request is recorded, the microphone's level stands as a row of blocks at the top, as over the Pokédex's result, and the page begins under that. With the keyboard up, the keys sit in the bottom band chat's keyboard uses, and the page fills the space above. One layout serves the browser and the Pokédex (`PageId`, the `paged_*` functions of `ui/view/rooms.rs`).

**Pokédex** (`Screen::Pokedex`): the artwork centred at the top, its side the inset width of the area under the bar (464 px, 382 px in Rail's pane) divided by φ (287 px, 236 px), its text one gap below, set like the text screen (`TextRows`, `paint_artwork_over` in `ui/textroom/mod.rs`). The wheel scrolls both. While a name is recorded, the microphone's level stands as a row of blocks at the top, in the small writing's line with half a line under it, and the artwork and text begin under that. With the keyboard up, the keys sit in the bottom band chat's keyboard uses, and the result fills the space above.

## 9. The robot

The robot is a drawing of its own (`crates/funcore/assets/icons/robot.svg`): a square head, two round eyes, a mouth of teeth, and an ear on each side carrying an antenna with a ball. `r1_funcore::ui::robot` animates its paths `head`, `antenna-left`, `antenna-right`, `eye-left`, `eye-right` and `mouth`. Eyes close to dashes or open wider than drawn; pupils shrink and move; the mouth closes, opens wide or smiles; antennas bend from their base; the head shifts up to 1/32 of the drawing's box (`SHIFT`), tilts up to 0.1 rad (`LEAN`) and stretches up to 8 % (`STRETCH`).

**Seats** (`Ui::robot_seat`). The robot is in every frame of every screen and moves between seats on `SEATING`:

- home: where the theme seats it (centre of the main stage, above the menu, on Deck's chat card, which stands on every page of home, in Rail the bar);
- Companion's chat while not muted: large over the conversation (`Ui::is_chat_robot_large`);
- the passphrase's, the key's and the bell's window and the window of a call that rings, while it shows: the right end of its bar;
- everywhere else: the right end of the bar.

It is always drawn in the mark colour: at home, in the bar, asleep in place while switched off, as its switch's icon, at the top of a PDF page. Its switch is hidden (`Presence::Never`); the saved setting `robot` still switches it, and while it is off the main stage shows a sleeping robot as a plain icon. Nothing the robot does counts as use, nor does a lift, a tilt or a glow. It falls asleep after 90 s without input (`ASLEEP_NS`), so only on a cable: off it the device sleeps first, after 1 min.

**Activities** (`Activity`). The robot shows what the device is doing. While an activity lasts, the robot holds a pose, opens with a gesture, and plays beats between rests, picked by weight. An activity never interrupts one of the robot's idle gestures, and no activity lets it doze. If two apply, the first of filming, speaking, listening, searching, painting and thinking wins (`Ui::activity` in `ui/screen/seat.rs`).

| Activity | While | Pose | Opening | Beats (weight); rest between |
|---|---|---|---|---|
| listening | the radio scans for networks or tests one, the network is asked for the time, a note, a chat message, a request to the browser, to the bell or for the rundowns or a Pokédex name is recorded, a note plays, the translator's model listens | head to one side, antennas up | starts, eyes wide, antennas apart | antennas ping apart and back (4), a ping and a blink (2), a nod (1); 1.5 to 3 s |
| speaking | a text is read aloud, a chat reply is spoken, the translator's model speaks | a little taller | draws breath, mouth shut | a phrase of syllables (4), a short phrase (3), a question (1), a phrase ending in a grin (1), a blink (1); 1 to 2 s |
| thinking | a model transcribes, summarises, gathers news, writes, answers in chat, takes a step for the browser or reads a request to the bell or for the rundowns; chat wakes; a voice or translator session is connecting; an OpenAI key is being checked | eyes up and away, head turned the other way, mouth small | rings from its antennas | rings (3), eyes drifting across with the head following (1), a "hm" with a nod (1), an idea that jerks an antenna (1); 1.5 to 3 s |
| searching | chat's model searches the web; the browser fetches a page; the device reads the feeds a request for the rundowns adds; the Pokédex looks up a name | eyes wide, pupils small, looking straight ahead, a little taller | looks both ways | looks to one side, sweeps to the other and back (1); 0.3 to 0.6 s |
| painting | the pixel cam redraws a photo | eyes narrowed on the progress row where it shows | a nod, drawing itself up | a narrower squint, leaning in (2), a look from the work to the picture and back (2), a blink (1), and a small nod with a smile per block lit; 1.5 to 3 s |
| filming | a video is recorded | left eye narrowed as through a viewfinder | screws that eye up | peers harder, leaning in, the other eye wide (3), a blink (1); 1.5 to 3 s |

**Rings** (thinking, `RIPPLE`). A ring is the antenna ball's outline grown about its centre, its stroke thinning from the drawing's line width (the antenna stalk, 31 of 960) to nothing. It leaves the ball fast (`Easing::Snap`) and slows until it reaches the head, about 2.5 ball radii out. A wave sends two rings from each ball, 0.3 s apart; a ring lasts 0.9 s, a wave 1.2 s. Rings follow the antennas however they bend or turn.

**Touch on the robot** (`Ui::act_on_robot` in `ui/screen/seat.rs`), acted on when the finger lifts:

| Screen | Touch | Next touch |
|---|---|---|
| home, every theme | opens chat | |
| photos, grid or shown large | shows them the other way (pixel art or as taken) | the same |
| photo viewfinder, with the camera tool on | takes a photo for the robot alone; the conversation opens with it as it comes ([Chat](chat.md#photos)) | nothing until it has come |
| text screen, on a news rundown or a note's or translation's summary | reads aloud from the block at the top of the screen | pauses; the touch after resumes |
| browser, a page showing | reads the page aloud from the block at the top of the screen | pauses; the touch after resumes |
| chat | mutes | unmutes |
| a call, while it stands | mutes the microphone | unmutes it |
| Pokédex | shows the keyboard | returns to voice |
| a Signal conversation | shows the keyboard | returns to voice |
| anything else (a transcript, a translated conversation) | nothing | |

On a Signal conversation a finger held on the robot for 3 s switches auto mode (`HOLD_NS` in `ui/screen/seat.rs`, a design choice; [Signal](signal.md#auto-mode)). At home a finger that lands on the robot makes it giggle; a finger that lands elsewhere at home draws its eyes for 0.61 s (`NOTICE_NS`), allowing for its tilt. A sleeping robot only wakes.

**Glow** (`Robot::beckon`). Only where a touch on the robot does something, the antenna balls fade from the mark colour to the interface colour in 0.5 s and back in 0.7 s (`GLOW_UP_MS`, `GLOW_DOWN_MS`): when the robot arrives there, and whenever 10 s have passed since both the last glow and the last input (`GLOW_PERIOD_NS`), also while asleep. The stalks keep the mark colour (`Piece::Tip`). The glow runs on its own clock and never delays a move. The body is drawn first and the tips blended over it by coverage (`Pen::cover_over`), so tip and stalk meet without a seam.

**Behaviour by screen** (`Scene`, set in `Ui::brief_robot`). `Free` (its mood decides): home, the stages in Stage, brightness, volume. `Calm` (small movements only, eyes on the selected item, the key under the finger, the time, the newest row or the page centre): every other screen. `Rapt` (only blinks, never dozes): while a note records or plays, a video records or plays, or a text or the browser's page is read aloud. On brightness its eyes narrow as the light goes up and widen in the dark. Asleep, its eyes and mouth are closed over a faint smile, its head rests to one side, sunk, antennas hanging. A new screen states what the robot does there.

**How it moves.** A gesture is a list of moves with waits between them; nothing is drawn during a wait. Each move uses one easing (`Easing`): `Even`, `Snap`, `Fall`, `Spring` (about 25 % overshoot) or `Steady`. In the usual mood just over half of the idle gestures, by weight, move the whole body. An activity's beat modifies the pose the activity holds (`Look::on_base`), so a blink while listening keeps the listening head. The eyes reach their target in 160 ms (`TURN_MS`); a glance at the stage holds 450 ms (`GLANCE_HOLD_MS`).

**Rules** (`shot robot`, `shot gesture` and `shot budget` show and measure them):

- Every move carries some part of the robot at least as far as the head's largest shift (`SHIFT`: 3.9 px in the bar, 13 px at Stage's home), or emits rings or z's, so nothing is lost in the bar.
- Each activity has a beat that no other activity and no idle gesture has.
- Over an hour, stirred every 30 s, the robot keeps the display out of idle 19 to 23 % of the time on its own and 20 to 24 % asleep; under an activity at least 19 % and at most 23 %, 36 % while speaking or thinking, 80 % while searching.
- No idle gesture redraws for more than 1.1 s, except the nap (6.2 s: a breath and a start). A beat, reaction or opening lasts 2.2 s at most; falling asleep 2.3 s plus a breath; a move while asleep at most one breath (3 s).
- No move takes an antenna more than 1/16 of the box beyond rest (`ANTENNA_BEND`), nor the head more than 1/32 (`SHIFT`). An antenna never bends past its maximum, however beats add up. The face stays inside the head, the antennas beside it, and rings end where the head begins; only z's and rings go further.
- An activity starts only once an idle gesture has ended. A glow never delays a move.
- The antenna tips glow, in every theme, only where a touch on the robot does something; the stalks keep the mark colour.

**Triggers.** The robot has 76 gestures (`gestures.rs`); an activity's opening and beats are in the activities table.

| Trigger | Animation |
|---|---|
| A rest ends with no activity, every 2 to 5 s | one of 30 idle gestures (`IDLE`), by weight per mood and screen: usual; worried while the battery is below one bar off the cable (darts, chattering, gulps, frightened looks at the battery, never cheers or giggles); drowsy after 45 s without input; `Calm`; `Rapt` (only blinks) |
| 90 s without input | falls asleep: yawns, eyes droop, open once more and close, a sigh |
| A rest ends while asleep, every 4 to 9 s | one of seven by weight (`SLEEPING`): a breath with three z's leaving through the top within 3 s (most often), a snore, a dream smile, sleep talk, an antenna twitch, snuggling down, one eye peeping |
| Touch, button, wheel, a lift, news or an activity while asleep; the device woken by the button | wakes with a start, looks both ways, relieved |
| The device woken by a cable | blinks awake, looks at the battery, jumps for joy |
| A cable plugged in; unplugged; the battery reaching the cable's goal (once per cable) | looks at the battery and jumps for joy; swallows; sighs contentedly |
| Picked up after lying still, at home with the screen on | jumps with fright, looks down, grins at its holder, waves its antennas |
| A progress-row block lights | a small nod with a smile |
| A network remembered or made default; the clock set right; a note, translated conversation, transcript, summary or rundown saved; an OpenAI key saved; a tunnel's config kept; a page of the browser's come; a Pokédex result found; the SIM's PIN typed; a request on the bell's list that changed the bell; a rundown a request kept, a deletion a request asked made by its click, and one of the two a new device has kept again | nods |
| A network or all networks forgotten; a fact, a day or the thread of the robot's forgotten; a roll item, note or translated conversation deleted; an alarm, reminder or timer cancelled by hand; a request on the bell's list that left the bell as it stood; a deletion a request asked of the rundowns refused, a rundown's feed removed or a rundown deleted by hand; a scanned code with no key, or with no config the router takes | shakes its head |
| A join failed; no time received; a photo not redrawn; reading aloud or a translator session failed; a notes or translations request failed; an edition not made while the news shows; a chat turn failed; a key check that saved nothing; a tunnel's config that could not be kept; the saved key refused; a visit of the browser's failed; a request to the bell or for the rundowns failed; a Pokédex lookup that found nothing | slumps with a frown |
| A photo taken | squeezes its eyes shut, peers out of one, glad |
| Pixel art saved | jumps with a grin |
| The debug screen unlocked | winks |
| A theme or palette chosen, by hand or by chat; given a name | the admire animation: looks itself over and shimmies (`Reaction::Admire`) |
| It has learned enough about the user | cheers, two jumps |
| Switched on for its first chat | eyes open halfway, close, open wide, looks both ways, glad |
| Muted, or the browser's or the Pokédex keyboard shown; unmuted, or the keyboard hidden | lips pressed, antennas folded back, eyes on the keys, held while the keys show; clears its throat and grins |
| A finger lands on it at home | giggles, eyes squeezed shut |
| A touch on it over the photos | one of six glad gestures at random (`POKED`) |
| Wheel or button on a stage other than home, the wheel moving it off home, back from a stage; a stage opened behind an icon | glances at the middle; at the docked icon |
| Its switch (hidden, so never triggered) | a jump with a grin |

The robot's numbers are design choices: every rest, weight, pose and time in `ui/robot/`, including the antenna ping (0.5 of the maximum bend, `PING_SPREAD`), the extra height when speaking or searching (0.2, `SPEAK_STRETCH`), the squints (`PAINT_SQUINT` 0.75, `FILM_SQUINT` 0.55), the search scan (0.25 s there, 0.15 s hold, 0.4 s across, 0.15 s hold, 0.25 s back), and the lift detection ([System](../firmware/system.md#accelerometer)).

## 10. Controls

| Input | Action, on every screen |
|---|---|
| Wheel, one step | Moves what the screen is about by one: the next icon, picture, line or key layer; brightness, volume or the interface's sounds (up is brighter and louder); the next clock offset or one step of the selected time part; the value of a setting staged on its line; the other camera; 4 lines of text. Turning up moves content up, the next item arriving from below; turning down goes back (stage icons, text screen, translator, chat, an item shown large, key layers). In a list or grid it moves the selection frame: turning down selects the line below. A step against the previous direction always counts. |
| Button, click | Acts on what is shown: opens, takes the photo, plays, ends or sends typed text, starts or stops a recording or session, applies a theme or palette, turns a switch, stages a setting's line and sets what it shows (see 8); on the clock screen it hands the wheel the next part of the time, on the volume screen the other row, each rolling into Dial's band. With nothing to act on, the content swings. |
| Button, held at home, on a stage or a menu, or on a screen where a hold does nothing else | Talks, in every theme. Recording starts when the button goes down; the press is a click until held 382 ms (`HOLD_TO_TALK_NS`), longer than any click by hand (300 ms at most). Then chat opens, and what was said since the press is sent on release; back returns to the screen held on. In Rail the rail stands on `CHAT` meanwhile, and back returns it to the line it stood on; while the rail has the wheel, a hold beside the conversation, the Pokédex, the browser or the bell's list in the pane is the button held on it, which takes the wheel. The press that wakes the device talks the same way, and so does the press that lights a screen darkened during a task, for the voice a hold on that screen records for. |
| Button, held on the bell's list or the rundowns' | Records from the press for that list, not for the robot, once held 382 ms: the selection goes to the list's first line, which shows the microphone's level, and what was said is sent on release and sets or cancels a ring, or adds or changes a rundown. A click on that first line records until the next click ([Interface](interface.md#the-bell), [Rundowns](interface.md#rundowns)). |
| Button, held on chat, the browser, the Pokédex or a Signal conversation in voice mode | Records while held, sends on release. On the browser a press let go before 382 ms is a click, which raises the keys, and what it recorded is given up; a press there ends a reading aloud of the page, which the microphone would hear. |
| Button, held in the video viewfinder, the translator, a keyboard, the key, safe or bell window, a call or the window of one that rings, or a note recording or playing | Nothing until release, which is a click. No hold puts the device to sleep. |
| Button, let go over another screen than the one it went down on | No click. A screen that came while the button was down, by a swipe, by the conversation or by the key window, takes nothing from the release: what the press was for is let go as a sleep lets it go (`Ui::let_go`), what chat recorded sent, what the browser, the Pokédex, the bell's list, the rundowns' or a Signal conversation recorded given up; a press at home whose screen another took opens nothing. |
| Touch on the robot, lifted there | see 9 |
| Touch on a stamped icon, lifted there | arms it; a second touch acts; `send` acts at the first |
| Touch on home's battery, or on the foot of Rail's rail, with a cable attached, lifted there | A battery held at a computer port's ceiling below full is charged to full; one charging to full after such a touch is held where it is, green again. |
| Touch on a key | types, when the finger lifts, the key it ends on; held, a character offers its other forms |
| Touch anywhere else | Nothing. Lines, tiles, cards, palettes, thumbnails, docked icons and the clock's parts are worked by the wheel and the button; a finger moving over a screen where touch acts on lift changes nothing. |
| Button on a screen darkened during a task, or a cable plugged in | Turns the screen on. The press clicks nothing; held 382 ms it talks as a hold on that screen does, on chat, the browser, the Pokédex and a Signal conversation too, recorded from the press on. Touch and the wheel are off with the panel ([Interface](interface.md#light-and-sleep)). |
| Button while the bell rings | Silences it and does nothing else, nor does its release; the bell's window stays, and a click or a swipe from the left then lets it fall ([Interface](interface.md#the-bell)). |
| Swipe in from the left edge | Back one step, from anywhere. A click also goes back where the button has nothing else to do (an item shown large, brightness); on the text screen only the swipe does. A finger that came down on the keys is never a swipe. |
| Swipe down from the top edge | Home: in Stage the main stage on the robot, the current stage's icons dropping off the bottom; in the menu themes home's menu, first line selected. |
| 1 min without input or activity | Sleep, on any screen; a press wakes the device onto the same screen ([Sleep and clock](../firmware/sleep.md)). |

A new screen takes its controls from this table and adds none. One screen takes fewer: while no OpenAI key is saved, the key window ignores a swipe from the left on its lines and a swipe down from the top, and what the conversation asks opens no screen over it ([Interface](interface.md#the-openai-key)).

## 11. On-screen text

- On-screen text is made only in funcore, in `ui/` (`ui/words.rs`, each screen's constants, `BarWords`), short and uppercase: a screen's name, a state (`LISTENING`, `JOINING`, `COUNTING`, `ASKING`, `SAVED`, `WAITING`, `RECORDING`, `TRANSCRIBING`, `SUMMARIZING`, `GATHERING`, `WRITING`, `CONNECTING`, `READING`, `PAUSED`, `RINGING`, `NO PICTURES`, `NONE YET`, `NONE SET`, `NONE KEPT`, `NO NETWORKS`, `HOLD TO TALK`, `TYPE A NAME`, `TYPE A MESSAGE`, `TYPE WHERE TO GO`, `MUTED`, `THINKING`, `SEARCHING`, `SPEAKING`, `WAKING UP`, `SCANNING`, `CHECKING`, `KEEPING`, `KEY SET`, `CONFIG SET`, `KEPT`, `CLICK TO SET`), or a failure (`WRONG PASSWORD`, `NOT RECORDED`, `NOT HEARD`, `NO NETWORK`, `NO OPENAI KEY`, `LIMIT REACHED`, `REFUSED`, `NOT WRITTEN`, `NO ANSWER`, `NO FEED FOUND`, `NOT A KEY`, `KEY REFUSED`, `NOT A CONFIG`, `NOT KEPT`).
- One doing, one word, on every screen: the microphone heard for a request `LISTENING` with its time, and a note recorded `RECORDING`; speech the speech model writes down `TRANSCRIBING`; a text the voice model reads out `READING`, the chat's reply as a news rundown; the translator's live voice `SPEAKING`; a service being reached `CONNECTING`; the keys up `TYPE A NAME`, `TYPE A MESSAGE` or `TYPE WHERE TO GO`, and `MUTED` where the robot is.
- In Companion the robot speaks the bar subtitle and home's greeting as sentences (`write_robot_sentence`): capital first letter, final full stop, uppercase words lowercased except units and names (`MAH`, `UTC`, `WiFi`, `OpenAI`; `ACRONYMS`, `RESPELLINGS`), a figure alone without a stop.
- What goes on ends in an ellipsis (`…`) in every theme, in place of Companion's stop: a word of doing alone or with its particle (`CHECKING…`, `POWERING OFF…`, `Transcribing…` in Companion; `is_ongoing`, `write_status`). A thing (`EVERYTHING`), a word of doing before another word (`PACING TEST`) and a figure (`LISTENING 0:04`) are not.
- The user's and the world's text is shown as is: network names, file names, every word of a text.
- A log line names a network, never its password or a key.

## 12. Themes

`THEMES` offers six themes; a new device uses Dial (`Theme::STANDARD`). A theme (`Theme` in `r1_funcore::gfx::theme`) is a layout (`Layout` in `r1_funcore::ui::layout`): it gives each element the style values of its look (`Look`) and places the gauges and the clock. Screens map state to elements and input to actions; no screen paints, branches on a theme or reads the layout, and no element knows a screen or a theme. Themes adapt to the device, never the reverse.

Rules for every theme:

- One screen tree: every theme presents `TREE` (`ui/screen/tree.rs`) unchanged; home is its top level (`Ui::draw_home` in `ui/view/home.rs`), and a node added to the tree shows in every theme. Companion's home suggests three things and the way to everything (`SUGGESTED`).
- Grids as in 8: equal cells, a fixed number per page, nothing depending on the item count but on Deck's board, whose pages fill their room. More to follow is told one way per theme (`ListStyle.has_arrow`, `BarStyle.has_pager`), for a page and a text alike: the arrow down in the room a page leaves where it holds it and the scrollbar beside a text, or at the left of Terminal's status line `--More--`, over a text with how much has shown (`--More--(28%)`).
- Lines by role: one form per role in each theme, which gives the line its shape and its painter (`ListStyle::form_of`, `Item::paint`). Rows and entries share their page's height; Terminal lines keep their own. A line that opens a page differs from one that only informs by its marker alone.
- Text: every screen states once what it shows (`BarWords`, see 3); every line shows what its item holds now through one function for all themes (`write_node_word`, [Interface](interface.md#home-and-the-menus)); long fields scroll (see 8). Companion reads a menu of a few lines as a choice between them.
- Deck lists sit on the 2x4 board, each page filling its room, home's card pinned (see 8).
- The wheel on `COLORS` draws the screen and the robot in the palette it is on, until a click applies it or the screen is left.
- One selection marker per theme: a bold frame, a thin frame on cards, reverse video in Terminal, the band in Dial; in Rail a bold frame where the wheel is, the rail's thin while the pane has the wheel, and nothing marked or lit in the pane while the rail has it.
- Text is inset from its container: a bold line plus a gap in a bold frame (`Ruler::framed_inset`), a thin line plus a gap on a card (`Ruler::card_inset`), Terminal's own margin.
- A theme's card is its home as that theme draws it, scaled down (`Ui::paint_home_of`).
- Each theme has its own transition (see 6).
- Sections 5, 7, 9 and 10 hold in every theme: sizes; single-value screens; the robot at home turning in a clear square, but in Rail, whose robot stays in the bar; touch on the robot and its glow; touch, button and sleep.

**Rail.** Every level is one of two kinds. A level whose lines each open something, a screen or a level, none setting a value or only telling (home, `CAMERA`, `MIC`, `SETTINGS`, `FEATURES`), stands at the left once entered as a rail of its icons, a cell a line (`Shape::Rail`, `Form::Icon`, `Layout::home_grid`), a rule between it and the pane; everything else stands in the pane right of it (`Ruler.left`), a level of settings as lines. The rail shows the deepest such level on the way in; such a level shows in the pane as lines and on the rail as icons, its grid laid out where it stands (`Ui::seat_level`). Under its icons stand the battery, as tall as the two small icons right of it, the network's mark (WiFi's, or mobile data's over the modem) over the bell, the first that shows at the top, the three drawings beginning at one top and ending at one foot and centred on the rail as one, and under them the time, or the time the timer has left, on every screen the rail shows (`CornerStyle::Rail`). The pane follows the rail, at home as on every level (`ui/view/rail.rs`): while the rail turns it shows the line's icon large, its name and word in the bar; once the rail's frame rests it opens what the line opens, as a click would, but for the wheel, which stays with the rail: a screen opens in the pane (the conversation, Signal's conversations or link window, the editions, the browser, the bell's list, the Pokédex, the networks), and a level shows its lines there (`CAMERA`'s, `FEATURES`'). The viewfinders open on a click alone, as they power the camera. A click on the rail gives the wheel to what the pane shows, its keys rising where it has them up; a level that stands on the rail takes the rail's place instead, its lines leaving the pane for the rail, and the pane follows them. Back gives the wheel to the rail again; back on the rail puts its lines in the pane again, the level before it back on the rail, at home nowhere. A branch in the pane's lines (`TOOLS`) opens in the pane. While the rail has the wheel, a hold talks as from home, but beside the conversation, the Pokédex, the browser or the bell's list in the pane, where it is the button held there and gives it the wheel; the conversation a hold or a photo for the robot opens over what shows stands the rail on `CHAT` until back returns to what showed and the rail to the line it stood on. The robot sits in the bar, at home too. A level of settings never stands on the rail: its lines show in the pane with their values, a setting set on its own line. Rows in the pane have no icon, no chevron and no rules. The rail gives way to what takes the whole screen: a viewfinder, a picture or video shown large, the key, passphrase and bell windows, and the keys while they are up, what is typed above them breaking into lines as wide as the pane.

| | Stage | Dial | Deck | Terminal | Companion | Rail |
|---|---|---|---|---|---|---|
| Concept (`theme_summary`) | `ONE LARGE ICON AT A TIME` | `THE WHEEL ROLLS EVERYTHING` | `EVERYTHING ON A CARD` | `MENUS MADE OF TYPE` | `THE ROBOT LEADS` | `THE RAIL KEEPS YOUR PLACE` |
| Home | the main stage, the robot in the chat icon's slot | the robot, the top level on the dial below, the time large over the date, the battery at the bottom with the network's mark beside it in a network | the time, date, the network's mark in a network and battery over the chat card, which stands on every page; the other top-level lines as tiles, 2 across, a page's tiles filling the room under the card | the robot over `HOLD TO TALK`, a rule, the top level as lines at the bottom, the status line | the robot over its two-line comment, the three suggestions as rows, then the way to everything, the time large over the date, the battery at the bottom with the network's mark beside it in a network | the top level as the rail, the battery beside the network's mark over the bell, and the time, under its icons; the pane follows the rail as on every level, the conversation on `CHAT`, `CAMERA`'s lines on `CAMERA`; the robot in the bar |
| Content starts | at the top | around the central band | at the top | at the bottom | at the top | at the top, right of the rail |
| Lists | rows between rules | the dial: the selected line large in a band, neighbours as names fading with distance | the board: a line on a card of two cells side by side; menus below home one tile per cell | full-width lines, a dotted leader to the value, the selection in reverse with its cursor | rows with icons in the colour of marks, on no card | rows in the pane, with no icon, chevron or rule; a level whose lines each open something, once entered, as the rail, an icon a cell |
| Primary figure | in the bar | a line, large in the band | a 4-cell card under its screen's icon: the value twice as large beside a 10-block gauge, a gap above what follows from it; a single-value screen is one card over the whole room | a line, label over value | three times as large, what follows from it beside it, a 20-block gauge below where it has a level | in the bar |
| Figures | rows | rows | one cell each, at its foot: label, value, what follows, a chevron where it opens a page | label over value | two columns | rows |
| Actions | rows under `WAYS DOWN` | rows | tiles as a menu's, one cell each | `[ BUTTONS ]` at the left of their places, in a row, one to a line where names do not fit | rows | rows under `WAYS DOWN` |
| Entries | rows | rows | 2-cell cards: name, length, two lines of the text's start | a table under `WHEN LEN WHAT` | behind its kind's mark, the name's last word dimmed, sharing the page | rows |
| Rolls | a 2x3 grid | a drum | a 2x3 grid | a grid, the selected picture under a band in reverse | a 2x3 grid | a 2x3 grid |
| More follows (`ListStyle.has_arrow`, `BarStyle.has_pager`) | marks: an arrow down in the room a page leaves, a scrollbar beside a text | marks, as Stage | marks, as Stage | the pager: `--More--` at the left of the status line while a page follows, `--More--(28%)` over a text | marks, as Stage | marks, as Stage |
| `COLORS` (drawn in the palette the wheel is on) | rows | rows | preview cards, two cells tall | letter, sample and swatch | rows behind the robot | rows |
| Chat | a log down the left | the user's messages at the right, the microphone disc at the bottom | each message on a card, the user's at the right | each message after the speaker's name; in voice mode the user's prompt and cursor | the robot large above in voice mode, the user's messages at the right | a log down the left |
