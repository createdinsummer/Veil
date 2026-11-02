CREATE TABLE space_info (
 id TEXT PRIMARY KEY NOT NULL, name TEXT NOT NULL CHECK(length(trim(name))>0), description TEXT,
 local_path TEXT UNIQUE, storage_mode TEXT NOT NULL DEFAULT 'shared' CHECK(storage_mode IN ('shared','dedicated')),
 sort_order INTEGER NOT NULL DEFAULT 0 CHECK(sort_order>=0), created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL
) STRICT;
CREATE TABLE space_div (
 div_id TEXT PRIMARY KEY NOT NULL, space_id TEXT NOT NULL REFERENCES space_info(id), cached_name TEXT NOT NULL,
 sort_order INTEGER NOT NULL DEFAULT 0 CHECK(sort_order>=0), registered_at INTEGER NOT NULL, updated_at INTEGER NOT NULL
) STRICT;
CREATE INDEX space_div_space ON space_div(space_id,sort_order,div_id);
CREATE TABLE user_setting (key TEXT PRIMARY KEY NOT NULL,value_json TEXT NOT NULL CHECK(json_valid(value_json)),updated_at INTEGER NOT NULL) STRICT;
CREATE TABLE system_setting (key TEXT PRIMARY KEY NOT NULL,value_json TEXT NOT NULL CHECK(json_valid(value_json)),updated_at INTEGER NOT NULL) STRICT;
CREATE TABLE local_record (
 record_type TEXT NOT NULL,record_key TEXT NOT NULL,value_json TEXT NOT NULL CHECK(json_valid(value_json)),updated_at INTEGER NOT NULL,
 PRIMARY KEY(record_type,record_key)
) STRICT;
CREATE INDEX local_record_recent ON local_record(record_type,updated_at DESC,record_key);
