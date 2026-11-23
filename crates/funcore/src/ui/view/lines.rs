use crate::gfx::Rect;
use crate::gfx::motion::Edge;
use crate::gfx::palette::Roles;
use crate::gfx::pen::Pen;
use crate::gfx::store::Store;
use crate::ui::Ui;
use crate::ui::field::cells;
use crate::ui::gauge::{BlockRow, Gauge};
use crate::ui::grid::{self, Band, Grid};
use crate::ui::item::{Form, Item, Role, Table, band_blocks};
use crate::ui::kit::Kit;
use crate::ui::list::dial::{figure_band, line_band};
use crate::ui::list::{LinePicture, Lines, ListForm, Mark, Page, Primary, RowsStyle};
use crate::ui::picture::drum;
use crate::ui::screen::Screen;
use crate::ui::screen::bar::BarWords;
use crate::ui::screen::tree::Node;
use alloc::borrow::Cow;
use alloc::string::String;
use alloc::vec::Vec;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum List {
    Networks,
    Known,
    Cost,
    Spending,
    Share,
    Power,
    About,
    Colours,
    Debug,
    Memos,
    Memo,
    Signal,
    Calls,
    CallChoice,
    Call,
    Bell,
    BellEntry,
    Rundowns,
    Rundown,
    Menu(usize),
    Key,
    Ring,
    Incoming,
    Settings,
}

impl List {
    pub(in crate::ui) const SCREENS: [List; 23] = [
        List::Networks,
        List::Known,
        List::Cost,
        List::Spending,
        List::Share,
        List::Power,
        List::About,
        List::Colours,
        List::Debug,
        List::Memos,
        List::Memo,
        List::Signal,
        List::Calls,
        List::CallChoice,
        List::Call,
        List::Bell,
        List::BellEntry,
        List::Rundowns,
        List::Rundown,
        List::Key,
        List::Ring,
        List::Incoming,
        List::Settings,
    ];

    pub(in crate::ui) const fn of(screen: Screen) -> Option<List> {
        Some(match screen {
            Screen::Networks => List::Networks,
            Screen::Known => List::Known,
            Screen::Cost => List::Cost,
            Screen::Spending => List::Spending,
            Screen::Share => List::Share,
            Screen::Power => List::Power,
            Screen::About => List::About,
            Screen::Colours => List::Colours,
            Screen::Debug => List::Debug,
            Screen::Memos => List::Memos,
            Screen::Memo => List::Memo,
            Screen::Signal => List::Signal,
            Screen::Calls => List::Calls,
            Screen::CallChoice => List::CallChoice,
            Screen::Bell => List::Bell,
            Screen::BellEntry => List::BellEntry,
            Screen::Rundowns => List::Rundowns,
            Screen::Rundown => List::Rundown,
            Screen::Ring => List::Ring,
            Screen::Incoming => List::Incoming,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(in crate::ui) enum LineTag {
    Plain,
    Primary,
    Folded,
}

#[derive(Default)]
pub(in crate::ui) struct Lists {
    entries: Vec<(List, Lines)>,
    bands: Vec<Band>,
}

impl Lists {
    fn of(&self, list: List) -> Option<&Lines> {
        self.entries.iter().find(|entry| entry.0 == list).map(|entry| &entry.1)
    }

    fn of_mut(&mut self, list: List) -> &mut Lines {
        let at = self.entries.iter().position(|entry| entry.0 == list).unwrap_or_else(|| {
            self.entries.push((list, Lines::default()));
            self.entries.len() - 1
        });
        &mut self.entries[at].1
    }

    fn items_of(&self, list: List) -> &[Option<Item>] {
        self.of(list).map_or(&[][..], |lines| &lines.items[..])
    }
}

impl Ui {
    pub(in crate::ui) fn grid_of(&self, list: List) -> &Grid {
        match list {
            List::Networks => &self.networks.list,
            List::Known => &self.networks.page,
            List::Cost => &self.cost.grid,
            List::Spending => &self.cost.spending,
            List::Share => &self.cost.share,
            List::Power => &self.power.grid,
            List::About => &self.about.grid,
            List::Colours => &self.themes.palettes,
            List::Debug => &self.debug.grid,
            List::Memos => &self.memos.grid,
            List::Memo => &self.memos.sheet,
            List::Signal => &self.signal.grid,
            List::Calls => &self.calls.grid,
            List::CallChoice => &self.calls.choice,
            List::Call => &self.calls.lines,
            List::Bell => &self.bells.list,
            List::BellEntry => &self.bells.page,
            List::Rundowns => &self.rundowns.list,
            List::Rundown => &self.rundowns.page,
            List::Menu(level) => self.tree.levels[level].menu().expect("a menu's lines stand in its grid"),
            List::Key => &self.keying.grid,
            List::Ring => &self.bells.grid,
            List::Incoming => &self.calls.answers,
            List::Settings => &self.gauges.lines,
        }
    }

    pub(in crate::ui) fn grid_of_mut(&mut self, list: List) -> &mut Grid {
        match list {
            List::Networks => &mut self.networks.list,
            List::Known => &mut self.networks.page,
            List::Cost => &mut self.cost.grid,
            List::Spending => &mut self.cost.spending,
            List::Share => &mut self.cost.share,
            List::Power => &mut self.power.grid,
            List::About => &mut self.about.grid,
            List::Colours => &mut self.themes.palettes,
            List::Debug => &mut self.debug.grid,
            List::Memos => &mut self.memos.grid,
            List::Memo => &mut self.memos.sheet,
            List::Signal => &mut self.signal.grid,
            List::Calls => &mut self.calls.grid,
            List::CallChoice => &mut self.calls.choice,
            List::Call => &mut self.calls.lines,
            List::Bell => &mut self.bells.list,
            List::BellEntry => &mut self.bells.page,
            List::Rundowns => &mut self.rundowns.list,
            List::Rundown => &mut self.rundowns.page,
            List::Menu(level) => self.tree.levels[level].menu_mut().expect("a menu's lines stand in its grid"),
            List::Key => &mut self.keying.grid,
            List::Ring => &mut self.bells.grid,
            List::Incoming => &mut self.calls.answers,
            List::Settings => &mut self.gauges.lines,
        }
    }

    pub(in crate::ui) fn item_of(&self, list: List, index: usize) -> Option<Item> {
        let item = match list {
            List::Networks => self.network_item(index),
            List::Known => self.known_item(index),
            List::Cost => self.cost_item(index),
            List::Spending => self.spending_item(index),
            List::Share => self.share_item(index),
            List::Power => self.power_item_at(index),
            List::About => self.about_item(index),
            List::Colours => self.palette_item(index),
            List::Debug => self.debug_item(index),
            List::Memos => self.memos_item(index),
            List::Memo => self.sheet_item(index),
            List::Signal => self.signal_item(index),
            List::Calls => self.calls_item(index),
            List::CallChoice => self.choice_item(index),
            List::Call => self.call_item(index),
            List::Bell => self.bell_item(index),
            List::BellEntry => self.bell_entry_item(index),
            List::Rundowns => self.rundowns_item(index),
            List::Rundown => self.rundown_item(index),
            List::Menu(level) => self.menu_item(level, index),
            List::Key => self.key_item(index),
            List::Ring => self.ring_item(index),
            List::Incoming => self.answer_item(index),
            List::Settings => self.setting_item(index),
        }?;
        Some(if self.is_hero(list, index) {
            Item { name: self.chat.name.clone(), text: self.chat_last_words(), ..item }
        } else if list == List::Menu(0) && self.does_home_speak() {
            Item { word: String::new(), ..item }
        } else {
            item
        })
    }

    // The node a list's line stands for: a menu's line, or a setting's line of a screen's list.
    pub(in crate::ui) fn node_at(&self, list: List, index: usize) -> Option<&'static Node> {
        match list {
            List::Menu(level) => self.tree.levels[level].node(index),
            List::Cost => self.cost_node(index),
            List::Networks
            | List::Known
            | List::Spending
            | List::Share
            | List::Power
            | List::About
            | List::Colours
            | List::Debug
            | List::Memos
            | List::Memo
            | List::Signal
            | List::Calls
            | List::CallChoice
            | List::Call
            | List::Bell
            | List::BellEntry
            | List::Rundowns
            | List::Rundown
            | List::Key
            | List::Ring
            | List::Incoming
            | List::Settings => None,
        }
    }

    pub(in crate::ui) fn is_hero(&self, list: List, index: usize) -> bool {
        self.look().home.has_hero && list == List::Menu(0) && index == 0
    }

    fn line_form(&self, list: List, index: usize, item: &Item) -> Form {
        let style = self.look().list;
        match list {
            _ if self.is_hero(list, index) => Form::Hero(self.ruler.home_side()),
            List::Menu(level) if self.rail_wheel(level).is_some() => Form::Icon,
            List::Menu(_) if style.page == Page::Board => Form::MenuTile,
            _ if self.has_cards(list) => Form::PaletteCard(index),
            _ if style.page == Page::Board && item.role == Role::Plain && item.icon.is_some() && list != List::Key => {
                Form::ActionTile
            }
            _ => style.form_for(item),
        }
    }

    pub(in crate::ui) fn kit(&self) -> Kit<'_> {
        Kit {
            ruler: &self.ruler,
            roles: self.roles,
            glyphs: &self.glyphs,
            mask: &self.mask,
            ticking: &self.ticking,
            format: self.format,
            ids: &self.ids,
        }
    }

    fn kit_for(&self, item: &Item) -> Kit<'_> {
        self.kit().with_roles(item.palette.map_or(self.roles, Roles::at))
    }

    fn table(&self) -> Table {
        Table { has_lengths: self.do_entries_last() }
    }

    pub(in crate::ui) fn line_room(&self, list: List) -> usize {
        let room = self.grid_of(list).place_size().0 as i32 - 2 * self.ruler.framed_inset();
        cells(room, &self.ruler.body)
    }

    pub(in crate::ui) fn primary_item(
        &self,
        label: impl Into<String>,
        value: impl Into<String>,
        level: Option<u8>,
        text: String,
        icon: &'static str,
    ) -> Item {
        let item = Item::primary(label, value, level, text);
        if self.look().list.primary == Primary::Card { item.with_icon(icon) } else { item }
    }

    pub(in crate::ui) fn folding_item(
        &self,
        label: impl Into<String>,
        value: impl Into<String>,
        level: Option<u8>,
        fold: String,
        icon: &'static str,
    ) -> Item {
        self.primary_item(label, value, level, if self.does_primary_fold() { fold } else { String::new() }, icon)
    }

    fn is_primary_listed(&self) -> bool {
        self.look().list.primary != Primary::Hidden
    }

    fn does_primary_fold(&self) -> bool {
        matches!(self.look().list.primary, Primary::Card | Primary::Large)
    }

    pub(in crate::ui) fn is_tag_listed(&self, tag: LineTag) -> bool {
        match tag {
            LineTag::Plain => true,
            LineTag::Primary => self.is_primary_listed(),
            LineTag::Folded => !self.does_primary_fold(),
        }
    }

    pub(in crate::ui) fn primary_gap(&self) -> usize {
        usize::from(!self.is_primary_listed())
    }

    pub(in crate::ui) fn place_in(&self, list: List) -> Option<(usize, usize)> {
        let grid = self.grid_of(list);
        grid.items.filter(|count| *count > 0).map(|count| (grid.selection + 1, count))
    }

    pub(in crate::ui) fn style_grid(&self, grid: Grid) -> Grid {
        if self.look().list.is_at_foot { grid.at_foot() } else { grid }
    }

    pub(in crate::ui) fn list_grid(&self) -> Grid {
        let (room, height, gap) = (self.ruler.under_bar(), self.ruler.height, self.ruler.gap);
        let style = self.look().list;
        let rows = style.page_rows(&self.ruler);
        if style.form == ListForm::Typed {
            let line = self.ruler.line_icon_side();
            let foot = height - self.ruler.status_height() - gap;
            let area = Rect::new(0, foot - rows as i32 * line, self.ruler.width, rows as i32 * line);
            return self.style_grid(Grid::new(&self.ids, area, height, 0, 1, rows));
        }
        let grid = self.style_grid(Grid::new(&self.ids, room, height, gap, 1, rows));
        if style.page == Page::Board { grid.with_fill() } else { grid }
    }

    pub(in crate::ui) fn board_grid(&self, columns: usize, rows: usize) -> Grid {
        self.style_grid(Grid::new(&self.ids, self.ruler.under_bar(), self.ruler.height, self.ruler.gap, columns, rows))
    }

    pub(in crate::ui) fn roll_grid(&self) -> Grid {
        if self.has_drums() {
            return Grid::new(&self.ids, drum::band(&self.ruler), self.ruler.height, self.ruler.gap, 1, 1).with_roll();
        }
        self.board_grid(grid::ROLL_COLUMNS, grid::ROLL_ROWS)
    }

    pub(in crate::ui) fn does_cut(&self) -> bool {
        self.look().list.form == ListForm::Typed
    }

    pub(in crate::ui) fn has_cards(&self, list: List) -> bool {
        list == List::Colours && self.look().list.is_carded
    }

    // True for a typed list, whose chosen line shows reversed: the palettes keep each its own colours.
    pub(super) fn is_reversed(&self, list: List) -> bool {
        self.does_cut() && list != List::Colours
    }

    pub(in crate::ui) fn is_dial(&self, list: List) -> bool {
        self.look().list.form == ListForm::Dial && !matches!(list, List::Key | List::Ring | List::Incoming)
    }

    pub(in crate::ui) fn dial_list(&self) -> Option<List> {
        self.visible_list().filter(|list| self.is_dial(*list))
    }

    pub(in crate::ui) fn does_dial_roll(&self) -> bool {
        self.dial_list().is_some_and(|list| list != List::Settings)
    }

    pub(in crate::ui) fn stand_dial(&mut self, item: usize) {
        self.dial.roll = Edge::still(item as f32);
    }

    pub(in crate::ui) fn plan(&mut self, list: List) {
        let style = self.look().list;
        if matches!(list, List::Menu(_)) || self.has_cards(list) {
            return;
        }
        let count = self.grid_of(list).items.unwrap_or(0);
        let items: Vec<Option<Item>> = (0..count).map(|index| self.item_of(list, index)).collect();
        let mut lists = core::mem::take(&mut self.lists);
        let mut bands = core::mem::take(&mut lists.bands);
        lists.of_mut(list).plan(&self.kit(), &style, &items, &mut bands);
        self.grid_of_mut(list).set_bands(&bands);
        lists.bands = bands;
        self.lists = lists;
    }

    pub(in crate::ui) fn note_dealt(&mut self, list: List) {
        let round = self.grid_of(list).round;
        self.lists.of_mut(list).round = Some(round);
    }

    pub(in crate::ui) fn turn_lines(&mut self, list: List, detents: i32) -> bool {
        self.grid_of_mut(list).step(detents);
        self.lists.of(list).and_then(|lines| lines.round) != Some(self.grid_of(list).round)
    }

    pub(in crate::ui) fn wheel_lines(&mut self, list: List, detents: i32) {
        if self.turn_lines(list, detents) {
            if self.is_dial(list) {
                self.note_dealt(list);
            } else {
                self.deal_planned(list, |_| true);
            }
        }
    }

    // Deals a list's lines; true where one of them came out other than it stood.
    pub(in crate::ui) fn deal_lines(&mut self, list: List, is_dealt: impl Fn(usize) -> bool) -> bool {
        if self.is_dial(list) {
            return self.deal_dial(list, is_dealt);
        }
        self.plan(list);
        self.deal_planned(list, is_dealt)
    }

    // Shows a list's lines, on `first` or, where the list holds the line of what is staged, on that.
    pub(in crate::ui) fn show_lines(&mut self, list: List, count: usize, first: Option<usize>) {
        let first = self.staged_line(list, count).or(first);
        let grid = self.grid_of_mut(list);
        grid.begin();
        match first {
            Some(first) => grid.set_items_at(count, first),
            None => grid.set_items(count),
        }
        self.stand_dial(first.unwrap_or(0));
        self.deal_lines(list, |_| true);
    }

    // Lays a list that shows anew for a count that may have changed, on the line it stood on, dealt anew where it did.
    pub(in crate::ui) fn lay_lines(&mut self, list: List, count: usize) {
        let grid = self.grid_of_mut(list);
        if grid.items != Some(count) {
            grid.redeal();
            grid.set_items(count);
        }
        self.deal_lines(list, |_| true);
    }

    fn paint_line(&mut self, list: List, index: usize, item: &Item, spare: Option<LinePicture>) -> LinePicture {
        let form = self.line_form(list, index, item);
        let kit = if matches!(form, Form::PaletteCard(_)) { self.kit() } else { self.kit_for(item) };
        let is_reversed = self.is_reversed(list);
        LinePicture::paint(spare, &kit, (item, form, self.table()), self.grid_of(list).size_at(index), is_reversed)
    }

    pub(in crate::ui) fn show_settings(&mut self) {
        if self.dial_list() == Some(List::Settings) {
            let (count, selection) = self.settings_place();
            self.show_lines(List::Settings, count, Some(selection));
        }
    }

    pub(in crate::ui) fn deal_settings(&mut self) {
        if self.dial_list() == Some(List::Settings) {
            self.gauges.lines.choose(self.settings_place().1);
            self.deal_lines(List::Settings, |_| true);
        }
    }

    fn deal_planned(&mut self, list: List, is_dealt: impl Fn(usize) -> bool) -> bool {
        let mut store = core::mem::take(&mut self.lists.of_mut(list).store);
        let indexes: Vec<usize> = self.grid_of(list).visible().filter(|index| is_dealt(*index)).collect();
        let mut is_changed = false;
        for index in indexes {
            let (Some(item), Some(spot)) = (self.item_of(list, index), self.grid_of(list).spot_of(index)) else {
                continue;
            };
            let key = item.to_key(self.grid_of(list).size_at(index), &self.roles);
            if !store.holds(spot, key) {
                let spare = store.spare(spot);
                let picture = self.paint_line(list, index, &item, spare);
                store.put(&self.ids, self.grid_of(list), (spot, Some(key)), picture, false);
                is_changed = true;
            }
        }
        self.lists.of_mut(list).store = store;
        self.note_dealt(list);
        is_changed
    }

    pub(in crate::ui) fn store_of(&self, list: List) -> Option<&Store<LinePicture>> {
        self.lists.of(list).map(|lines| &lines.store)
    }

    pub(in crate::ui) fn advance_lines(&mut self, list: List, now_ns: u64) {
        if self.lists.of(list).is_some() {
            let mut store = core::mem::take(&mut self.lists.of_mut(list).store);
            store.advance(now_ns, self.grid_of(list));
            self.lists.of_mut(list).store = store;
        }
    }

    fn deal_dial(&mut self, list: List, is_dealt: impl Fn(usize) -> bool) -> bool {
        let count = self.grid_of(list).items.unwrap_or(0);
        let mut items = core::mem::take(&mut self.lists.of_mut(list).items);
        let is_resized = items.len() != count;
        items.resize(count, None);
        let mut is_changed = is_resized;
        for (index, line) in items.iter_mut().enumerate().filter(|entry| is_resized || is_dealt(entry.0)) {
            let item = self.item_of(list, index);
            is_changed |= *line != item;
            *line = item;
        }
        self.lists.of_mut(list).items = items;
        self.note_dealt(list);
        self.deal_band();
        is_changed
    }

    fn visible_dial(&self) -> Option<List> {
        self.dial_list().or(self.list_under().filter(|list| self.is_dial(*list)))
    }

    fn deal_band(&mut self) {
        let Some(list) = self.visible_dial() else {
            return;
        };
        let index = self.dial.band_item();
        let size = ((self.ruler.width - 2 * self.ruler.gap).max(0) as usize, self.band_of(list).max(0) as usize);
        let item = match self.lists.items_of(list).get(index) {
            Some(item) => item.clone(),
            None => self.item_of(list, index),
        };
        if let Some(item) = item {
            let key = item.to_key(size, &self.roles);
            let mut dial = core::mem::take(&mut self.dial);
            dial.take_band(&self.kit_for(&item), &item, ((list, index), size), key, list == List::Settings);
            self.dial = dial;
        }
    }

    pub(in crate::ui) fn deal_roll(&mut self) {
        if self.visible_dial().is_some_and(|list| !self.dial.is_in_band((list, self.dial.band_item()))) {
            self.deal_band();
        }
    }

    pub(in crate::ui) fn dial_aim(&self) -> Option<f32> {
        let under = self.list_under().filter(|_| self.look().list.form == ListForm::Dial);
        match self.dial_list().or(under) {
            Some(list) => Some(self.grid_of(list).selection as f32),
            None => self.does_drum_show().then_some(self.rolls.grid.selection as f32),
        }
    }

    fn band_of(&self, list: List) -> i32 {
        let is_figure = |item: &Item| matches!(item.role, Role::Figure | Role::Primary(_));
        let has_figures = self.lists.items_of(list).iter().flatten().any(is_figure);
        if has_figures { figure_band(&self.ruler) } else { line_band(&self.ruler) }
    }

    fn draw_band_blocks(&self, pen: &mut Pen<'_, '_>, index: usize, (x, y): (i32, i32)) {
        let Some(gauge) = self.setting_gauge(index) else {
            return;
        };
        let row = band_blocks(&self.kit(), figure_band(&self.ruler));
        let first = Rect { x: x + row.first.x, y: y + row.first.y, ..row.first };
        Gauge { is_set: true, ..gauge }.draw_blocks(pen, &self.roles, BlockRow { first, ..row });
    }

    pub(in crate::ui) fn draw_dial(&self, pen: &mut Pen<'_, '_>, list: List, area: Rect) {
        let (count, items) = (self.grid_of(list).items.unwrap_or(0), self.lists.items_of(list));
        let item_at = |index: usize| match items.get(index) {
            Some(item) => item.as_ref().map(Cow::Borrowed),
            None => self.item_of(list, index).map(Cow::Owned),
        };
        let draw_over = |pen: &mut Pen<'_, '_>, index, at| self.draw_band_blocks(pen, index, at);
        self.dial.draw(pen, &self.kit(), (list, count, self.band_of(list)), area, item_at, draw_over);
    }

    // The mark of a grid of pictures: none beside a rail that has the wheel.
    pub(super) fn cells_mark(&self) -> Mark {
        if self.does_rail_lead() { Mark::Nothing } else { Mark::Frame(self.ruler.lines().1) }
    }

    fn rows_style(&self, list: List) -> RowsStyle {
        let (style, (thin, bold)) = (self.look().list, self.ruler.lines());
        let role = self.lists.of(list).map_or(Role::Plain, |lines| lines.role_of(self.grid_of(list).selection));
        let (rail, is_aside) = match list {
            List::Menu(level) => (self.rail_wheel(level), level != self.tree.front()),
            _ => (None, self.does_rail_lead()),
        };
        let mark = match style.form {
            _ if self.is_reversed(list) => {
                Mark::Reversed { caret: self.does_caret_show().then_some(self.caret.is_lit) }
            }
            ListForm::Typed => Mark::Frame(thin),
            _ if rail == Some(false) => Mark::Frame(thin),
            _ if is_aside && rail.is_none() => Mark::Nothing,
            _ if style.is_carded || style.is_on_card(role) => Mark::Frame(thin),
            _ => Mark::Frame(bold),
        };
        RowsStyle {
            has_rules: style.has_rules && list != List::Menu(0) && !self.has_cards(list),
            has_arrow: style.has_arrow,
            mark,
            table: self.table(),
        }
    }

    pub(in crate::ui) fn draw_rows(&self, pen: &mut Pen<'_, '_>, list: List) {
        let (kit, grid, style) = (self.kit(), self.grid_of(list), self.rows_style(list));
        match self.lists.of(list) {
            Some(lines) => lines.draw_rows(pen, &kit, grid, &style),
            None => Lines::default().draw_rows(pen, &kit, grid, &style),
        }
    }

    pub(in crate::ui) fn draw_lines(&self, pen: &mut Pen<'_, '_>, list: List, name: &str, words: &BarWords<'_>) {
        pen.fill(pen.bounds(), self.roles.background);
        if self.look().list.form == ListForm::Dial {
            self.draw_dial(pen, list, self.ruler.room());
        } else {
            self.draw_rows(pen, list);
        }
        pen.fill(Rect::new(0, 0, self.ruler.width, self.ruler.bar()), self.roles.background);
        let words = BarWords { status: words.status.or(self.staged_word()), ..*words };
        self.draw_bar(pen, name, &words);
    }
}
