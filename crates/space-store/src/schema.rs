pub const VERSION: i64 = 1;
pub const CREATE: &str = r#"
CREATE TABLE roots(id INTEGER PRIMARY KEY, identity BLOB NOT NULL, policy BLOB NOT NULL);
CREATE TABLE scans(id INTEGER PRIMARY KEY, root INTEGER NOT NULL REFERENCES roots(id), generation INTEGER NOT NULL, status TEXT NOT NULL CHECK(status IN ('scanning','finalizing','published','cancelled','failed','interrupted')), next_sequence INTEGER NOT NULL DEFAULT 0, started INTEGER NOT NULL, started_nanos INTEGER NOT NULL, ended INTEGER, ended_nanos INTEGER, selected_path BLOB NOT NULL, metadata BLOB NOT NULL);
CREATE INDEX scans_retention ON scans(root,status,id);
CREATE TABLE publication(root INTEGER PRIMARY KEY REFERENCES roots(id), scan INTEGER NOT NULL REFERENCES scans(id));
CREATE TABLE entries(scan INTEGER NOT NULL REFERENCES scans(id) ON DELETE CASCADE, node INTEGER NOT NULL, parent INTEGER, native BLOB NOT NULL, ordinal INTEGER, display TEXT NOT NULL, kind INTEGER NOT NULL, identity BLOB, logical INTEGER, allocated INTEGER, charged_logical INTEGER, charged_allocated INTEGER, canonical INTEGER NOT NULL DEFAULT 1, data BLOB NOT NULL, PRIMARY KEY(scan,node), UNIQUE(scan,parent,native));
CREATE INDEX children ON entries(scan,parent,native,node);
CREATE INDEX children_ordinal ON entries(scan,parent,ordinal);
CREATE INDEX traversal ON entries(scan,ordinal);
CREATE INDEX hardlinks ON entries(scan,identity,ordinal);
CREATE INDEX largest_logical ON entries(scan,kind,canonical,charged_logical DESC,ordinal,node);
CREATE INDEX largest_allocated ON entries(scan,kind,canonical,charged_allocated DESC,ordinal,node);
CREATE TABLE totals(scan INTEGER NOT NULL REFERENCES scans(id) ON DELETE CASCADE,node INTEGER NOT NULL,logical INTEGER NOT NULL,allocated INTEGER NOT NULL,unknown_logical INTEGER NOT NULL,unknown_allocated INTEGER NOT NULL,errors INTEGER NOT NULL, estimated INTEGER NOT NULL,unknown_entries INTEGER NOT NULL,excluded INTEGER NOT NULL,boundary INTEGER NOT NULL, PRIMARY KEY(scan,node));
CREATE TABLE coverage(scan INTEGER NOT NULL REFERENCES scans(id) ON DELETE CASCADE,node INTEGER NOT NULL,data BLOB NOT NULL,PRIMARY KEY(scan,node));
CREATE TABLE errors(scan INTEGER NOT NULL REFERENCES scans(id) ON DELETE CASCADE,node INTEGER,data BLOB NOT NULL);
CREATE INDEX errors_node ON errors(scan,node);
PRAGMA user_version=1;
"#;
