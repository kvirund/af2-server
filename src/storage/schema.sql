CREATE TABLE IF NOT EXISTS accounts (
    id INTEGER PRIMARY KEY,
    login_key TEXT NOT NULL UNIQUE
);

CREATE TABLE IF NOT EXISTS characters (
    id INTEGER PRIMARY KEY,
    account_id INTEGER NOT NULL REFERENCES accounts (id),
    name TEXT NOT NULL,
    level INTEGER NOT NULL,
    experience INTEGER NOT NULL,
    money INTEGER NOT NULL,
    premium INTEGER NOT NULL,
    vip INTEGER NOT NULL DEFAULT 0,
    life REAL,
    stamina REAL NOT NULL DEFAULT 0,
    radiation REAL NOT NULL DEFAULT 0,
    food REAL NOT NULL DEFAULT 0,
    water REAL NOT NULL DEFAULT 0,
    work_points REAL NOT NULL DEFAULT 0,
    studying INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS items (
    id INTEGER PRIMARY KEY,
    character_id INTEGER NOT NULL REFERENCES characters (id),
    bag INTEGER NOT NULL,
    cell INTEGER NOT NULL,
    count INTEGER NOT NULL,
    durability REAL NOT NULL,
    template INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS fish (
    id INTEGER PRIMARY KEY,
    character_id INTEGER NOT NULL REFERENCES characters (id),
    species INTEGER NOT NULL,
    grams INTEGER NOT NULL,
    size_class INTEGER NOT NULL,
    list INTEGER NOT NULL,
    experience INTEGER NOT NULL,
    bait INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS records (
    character_id INTEGER NOT NULL REFERENCES characters (id),
    species INTEGER NOT NULL,
    grams INTEGER NOT NULL,
    PRIMARY KEY (character_id, species)
);

CREATE TABLE IF NOT EXISTS bag_extra (
    character_id INTEGER NOT NULL REFERENCES characters (id),
    bag INTEGER NOT NULL,
    cells INTEGER NOT NULL,
    PRIMARY KEY (character_id, bag)
);

CREATE TABLE IF NOT EXISTS known_recipes (
    character_id INTEGER NOT NULL REFERENCES characters (id),
    recipe INTEGER NOT NULL,
    uses INTEGER NOT NULL,
    PRIMARY KEY (character_id, recipe)
);

CREATE TABLE IF NOT EXISTS skills (
    character_id INTEGER NOT NULL REFERENCES characters (id),
    perk INTEGER NOT NULL,
    level INTEGER NOT NULL,
    experience REAL NOT NULL,
    PRIMARY KEY (character_id, perk)
);

CREATE TABLE IF NOT EXISTS id_sequence (
    only_row INTEGER PRIMARY KEY CHECK (only_row = 1),
    next_id INTEGER NOT NULL
);

INSERT OR IGNORE INTO id_sequence (only_row, next_id) VALUES (1, 1);

CREATE INDEX IF NOT EXISTS characters_by_account ON characters (account_id);
CREATE INDEX IF NOT EXISTS items_by_character ON items (character_id);
CREATE INDEX IF NOT EXISTS fish_by_character ON fish (character_id);
