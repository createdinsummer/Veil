CREATE TABLE div_info (
 id INTEGER PRIMARY KEY NOT NULL CHECK(id=1), veil_id TEXT NOT NULL UNIQUE, name TEXT NOT NULL CHECK(length(trim(name))>0),
 created_at INTEGER NOT NULL, schema_version INTEGER NOT NULL CHECK(schema_version>0), data_keys TEXT NOT NULL CHECK(json_valid(data_keys))
) STRICT;
CREATE TABLE file_info (
 id TEXT PRIMARY KEY NOT NULL, parent_id TEXT REFERENCES file_info(id), name TEXT NOT NULL,
 kind TEXT NOT NULL CHECK(kind IN ('directory','file')), created_at INTEGER NOT NULL,updated_at INTEGER NOT NULL,
 cipher_name TEXT UNIQUE, size INTEGER CHECK(size>=0),key_id TEXT,nonce BLOB,encrypted_at INTEGER,version_history TEXT,
 CHECK((parent_id IS NULL AND name='' AND kind='directory') OR
       (parent_id IS NOT NULL AND length(name)>0 AND name NOT IN ('.','..') AND instr(name,'/')=0 AND instr(name,char(92))=0 AND instr(name,char(0))=0)),
 CHECK((kind='directory' AND cipher_name IS NULL AND size IS NULL AND key_id IS NULL AND nonce IS NULL AND encrypted_at IS NULL AND version_history IS NULL) OR
       (kind='file' AND cipher_name IS NOT NULL AND size IS NOT NULL AND key_id IS NOT NULL AND nonce IS NOT NULL AND length(nonce)=12 AND encrypted_at IS NOT NULL AND version_history IS NOT NULL AND json_valid(version_history) AND json_type(version_history)='array' AND json_array_length(version_history)>0))
) STRICT;
CREATE UNIQUE INDEX file_siblings ON file_info(parent_id,name COLLATE BINARY);
CREATE UNIQUE INDEX file_root ON file_info((1)) WHERE parent_id IS NULL;
CREATE TRIGGER file_parent_insert BEFORE INSERT ON file_info WHEN NEW.parent_id IS NOT NULL BEGIN
 SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM file_info WHERE id=NEW.parent_id AND kind='directory') THEN RAISE(ABORT,'parent must be directory') END;
END;
CREATE TRIGGER file_parent_update BEFORE UPDATE OF parent_id,kind ON file_info BEGIN
 SELECT CASE WHEN OLD.parent_id IS NULL AND (NEW.parent_id IS NOT NULL OR NEW.kind<>'directory') THEN RAISE(ABORT,'root is immutable') END;
 SELECT CASE WHEN NEW.parent_id IS NOT NULL AND NOT EXISTS(SELECT 1 FROM file_info WHERE id=NEW.parent_id AND kind='directory') THEN RAISE(ABORT,'parent must be directory') END;
 SELECT CASE WHEN NEW.kind='file' AND EXISTS(SELECT 1 FROM file_info WHERE parent_id=NEW.id) THEN RAISE(ABORT,'directory has children') END;
 SELECT CASE WHEN EXISTS(WITH RECURSIVE ancestors(id,parent_id) AS (SELECT id,parent_id FROM file_info WHERE id=NEW.parent_id UNION ALL SELECT f.id,f.parent_id FROM file_info f JOIN ancestors a ON f.id=a.parent_id) SELECT 1 FROM ancestors WHERE id=NEW.id) THEN RAISE(ABORT,'cycle') END;
END;
CREATE TRIGGER file_root_delete BEFORE DELETE ON file_info WHEN OLD.parent_id IS NULL BEGIN SELECT RAISE(ABORT,'root cannot be deleted'); END;
CREATE TABLE operation_locks (
 lock_id TEXT PRIMARY KEY NOT NULL,resource_type TEXT NOT NULL CHECK(resource_type='file'), resource_id TEXT NOT NULL,
 mode TEXT NOT NULL CHECK(mode IN ('read','write')),owner_identity TEXT NOT NULL CHECK(json_valid(owner_identity)),
 owner_endpoint TEXT CHECK(owner_endpoint IS NULL OR json_valid(owner_endpoint)),owner_token TEXT NOT NULL,expires_at INTEGER NOT NULL
) STRICT;
CREATE INDEX operation_resource ON operation_locks(resource_type,resource_id,expires_at);
