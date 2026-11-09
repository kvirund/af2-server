//! Message ids, named from the client's dispatcher. The same id can mean
//! different things in each direction.

pub const MONEY_CHANGED: i32 = 21;
pub const NPC_TALK: i32 = 60;
pub const LOGIN: i32 = 90;
pub const GO_TO_GAME: i32 = 100;
pub const CHARACTER_DETAILS: i32 = 100;
pub const CHARACTER_UPDATE: i32 = 102;
pub const LEVEL_UP: i32 = 103;
pub const CURRENCY: i32 = 105;
pub const PERSONAL_RECORD: i32 = 106;
pub const CHARACTER_LIST: i32 = 110;
pub const ENTER_WORLD: i32 = 111;
pub const TRAVEL_OPTIONS: i32 = 134;
pub const TRAVEL_START: i32 = 136;
pub const TRAVEL_ARRIVE: i32 = 137;
pub const TRAVEL: i32 = 138;
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
pub const ITEM_UPDATES: i32 = 181;
pub const ITEM_MOVE: i32 = 182;
pub const ITEM_SWAP: i32 = 184;
pub const HEARTBEAT: i32 = 255;
pub const FISH_SWAP_KEEPNETS: i32 = 285;
pub const FISH_RELEASE: i32 = 286;
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
pub const REPORTS_ONLY: [i32; 8] = [HEARTBEAT, 161, 1005, 1006, 1007, 1012, 1110, 1440];
