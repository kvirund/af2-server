//! Message ids, named from the client's dispatcher. The same id can mean
//! different things in each direction.

pub const MONEY_CHANGED: i32 = 21;
/// S→C a chat line; see `game::chat`.
pub const CHAT: i32 = 50;
pub const NPC_TALK: i32 = 60;
pub const DIALOG_ANSWER: i32 = 65;
/// Dialog text: string what to replace, string with what.
pub const DIALOG_REPLACE: i32 = 78;
/// The client asks with the fridge object's number and gets the window back.
pub const FRIDGE: i32 = 166;
pub const FRIDGE_PUT: i32 = 167;
pub const FRIDGE_TAKE: i32 = 168;
pub const DIALOG_TEXT: i32 = 70;
pub const DIALOG_OPTION: i32 = 72;
pub const DIALOG_OPEN: i32 = 74;
pub const DIALOG_CLOSE: i32 = 76;
pub const DIALOG_SPAN: i32 = 82;
pub const DIALOG_DATE: i32 = 83;
pub const LOGIN: i32 = 90;
pub const GO_TO_GAME: i32 = 100;
pub const CHARACTER_DETAILS: i32 = 100;
pub const CHARACTER_UPDATE: i32 = 102;
pub const LEVEL_UP: i32 = 103;
pub const CURRENCY: i32 = 105;
pub const PERSONAL_RECORD: i32 = 106;
pub const CHARACTER_LIST: i32 = 110;
pub const ENTER_WORLD: i32 = 111;
/// C→S: «Отмена» in the character creator. S→C (int): open the creator.
pub const CREATOR: i32 = 112;
/// C→S (string): is the name free? S→C (bool): it is.
pub const NAME_CHECK: i32 = 115;
/// C→S: «Создать» in the character creator: string name, int 0, int sex, then
/// the appearance bytes. S→C (int): too many characters.
pub const CREATE_CHARACTER: i32 = 120;
pub const TRAVEL_OPTIONS: i32 = 134;
pub const TRAVEL_START: i32 = 136;
pub const TRAVEL_ARRIVE: i32 = 137;
pub const TRAVEL: i32 = 138;
pub const WALK_ON: i32 = 139;
pub const WALKED_ON: i32 = 157;
pub const BASE_LIST: i32 = 145;
pub const BASE_DETAILS: i32 = 150;
pub const LOAD_BASE: i32 = 151;
pub const EXPERIENCE_GAINED: i32 = 155;
pub const SPAWN_POINT: i32 = 158;
pub const GO_TO_LOCATION: i32 = 160;
pub const FISH_REMOVE: i32 = 173;
pub const FISH_LIST_CLEAR: i32 = 174;
pub const FISH_LIST: i32 = 175;
pub const FISH_TO_SPARE: i32 = 176;
pub const FISH_TO_IN_USE: i32 = 177;
pub const ITEMS: i32 = 180;
pub const ITEMS_REMOVED: i32 = 190;
pub const ITEM_SPLIT: i32 = 200;
pub const BAG_PRICE: i32 = 210;
pub const BAG_BOUGHT: i32 = 215;
pub const REPUTATION: i32 = 30;
pub const FACTION_OPENED: i32 = 305;
pub const REPUTATION_LEVEL: i32 = 306;
pub const REPUTATION_GAINED: i32 = 307;
pub const CHARACTER_CARD: i32 = 32;
pub const RECORDS: i32 = 40;
pub const TITLES: i32 = 101;
/// S→C: the character died. Client: int 0, back to life.
pub const DEATH: i32 = 905;
/// Client: int title, the one to show.
pub const TITLE_CHOOSE: i32 = 102;
/// int title: the one shown now.
pub const TITLE_SET: i32 = 108;
/// int title: a new one earned.
pub const TITLE_NEW: i32 = 109;
pub const ACHIEVEMENTS: i32 = 130;
/// One deed record: a deed has opened.
pub const DEED_OPENED: i32 = 131;
/// One deed record: a deed is done (fanfare).
pub const DEED_DONE: i32 = 132;
pub const AUCTION: i32 = 330;
pub const ARTELS: i32 = 450;
pub const STATISTICS: i32 = 1150;
pub const STORAGE: i32 = 1230;
pub const VITALS: i32 = 1000;
/// S→C: weather (5 floats: clouds, fog, rain, snow, thunder), then int
/// radiation, int aggressive environment of the location for the bottom panel.
pub const WEATHER: i32 = 1001;
pub const ITEM_USE: i32 = 1320;
pub const PERKS_UPDATE: i32 = 1400;
pub const PERKS: i32 = 1401;
pub const PERK_STUDY: i32 = 1405;
pub const PERK_LEARNED: i32 = 1408;
pub const PERKS_QUIET: i32 = 1410;
pub const USER_PARAMS: i32 = 15;
pub const COURSE_STUDY: i32 = 275;
pub const CRAFT: i32 = 1700;
pub const CRAFT_STATION: i32 = 1701;
pub const RECIPE_LEARN: i32 = 1710;
pub const CRAFT_MAKE: i32 = 1702;
/// Case 1704: long item, shown in the "you got" window.
pub const ITEM_RECEIVED: i32 = 1704;
pub const USER_STATE: i32 = 1005;
pub const GATHER_POINTS: i32 = 161;
pub const GATHER_UPDATE: i32 = 162;
pub const HARVEST_START: i32 = 1500;
pub const HARVEST_FINISH: i32 = 1510;
pub const CRAFT_ERROR: i32 = 1703;
/// C→S: «Создать» in «Конструирование»: int station, int recipe, int extra
/// parts A, B, C, int modifiers, long the item built on (0: any). S→C: as 1702.
pub const CONSTRUCT: i32 = 1705;
pub const ITEMS_GAINED: i32 = 125;
pub const LOOT: i32 = 126;
pub const ITEM_UPDATES: i32 = 181;
pub const ITEM_MOVE: i32 = 182;
pub const ITEM_SWAP: i32 = 184;
pub const ITEM_MERGE: i32 = 195;
/// C→S long item: a rod broke, or a short line tore away whole.
pub const ITEM_BROKEN: i32 = 1305;
/// C→S long line, float metres torn off.
pub const LINE_TORN: i32 = 1306;
/// C→S long item: a piece of tackle went with a torn line.
pub const TACKLE_LOST: i32 = 1307;
/// C→S long character, string name, int 1 rod / 2 line, int template: the
/// news for other players that someone broke their tackle.
pub const TACKLE_NEWS: i32 = 910;
pub const HEARTBEAT: i32 = 255;
pub const ACTIVE_ICONS: i32 = 260;
/// C→S int 0 from the VIP window's button; S→C int 1 bought, -1 not enough ergs.
pub const VIP_BUY: i32 = 220;
/// C→S int tab: the board of tasks. S→C its offers; see `game::board`.
pub const BOARD: i32 = 700;
/// C→S int tab: give up that tab's tasks. S→C the tournaments.
pub const BOARD_REFUSE: i32 = 705;
pub const TOURNAMENTS: i32 = 705;
/// A tournament record: it has started for the player.
pub const TOURNAMENT_STARTED: i32 = 808;
/// A tournament record and its table rows.
pub const TOURNAMENT_TABLE: i32 = 810;
/// A tournament record: it is over.
pub const TOURNAMENT_END: i32 = 812;
/// A tournament record and its rows, in the result window.
pub const TOURNAMENT_RESULT: i32 = 818;
/// C→S long offer; S→C the answer as one `key=value` line.
pub const BOARD_TAKE: i32 = 710;
/// S→C {int command, long id} until the end: what happened to tasks.
pub const TASK_EVENTS: i32 = 670;
/// S→C long fish: taken by a task out of the keepnet.
pub const TASK_FISH: i32 = 672;
/// S→C all tasks, after clearing the old ones.
pub const TASKS: i32 = 675;
/// S→C tasks added or changed.
pub const TASK_UPDATES: i32 = 680;
pub const FISH_SWAP_KEEPNETS: i32 = 285;
pub const FISH_RELEASE: i32 = 286;
pub const REFINE: i32 = 290;
pub const BITE: i32 = 1120;
pub const FISH_LANDED: i32 = 1130;
pub const FISH_KEPT: i32 = 1135;
pub const FISH_KEPT_ALT: i32 = 1136;
pub const FISH_MARKET: i32 = 1200;
pub const FISH_PRICES: i32 = 1205;
pub const FISH_SELL: i32 = 1210;
pub const SHOP_LIST: i32 = 1300;
pub const SHOP_OPEN: i32 = 1309;
pub const SHOP_BUY: i32 = 1310;

/// Lowers the black overlay many requests raise while they wait for a reply.
pub const CLEAR_OVERLAY: i32 = 1310;

/// Client reports that need no reply, so they raise no overlay either.
pub const REPORTS_ONLY: [i32; 10] = [HEARTBEAT, 161, 1005, 1006, 1007, 1012, 1110, 1125, 1127, 1440];
