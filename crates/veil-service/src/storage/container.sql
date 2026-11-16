-- 文件作用：声明加密容器业务库版本 1 的固定表、索引与树约束触发器，供解锁后的初始化和结构校验使用。
-- 所属模块：service 容器存储；包含 div_info 身份/密钥、file_info 文件树、operation_locks 范围租约及 file_hash 摘要缓存。
-- 数据边界：SQLCipher 保护这些表的磁盘内容，独立 .enc 文件存放实际密文；全路径由父子关系组合，不冗余持久化。
-- 依赖能力：SQLite STRICT、JSON 函数、外键、递归 CTE、部分/表达式索引和触发器；密码及固定加密参数由 core 连接层提供。
-- 业务目标：约束单根、合法名称、有效父目录和不成环的树关系，并为密钥轮换与并发凭证提供统一记录；应用层仍须核验 UUID、版本、资源重叠和内容认证。
-- 加密容器根元数据：固定只有一行，存放容器身份、显示名、创建时间、结构版本和加密数据密钥目录。
CREATE TABLE div_info (
 id INTEGER PRIMARY KEY NOT NULL CHECK(id=1), veil_id TEXT NOT NULL UNIQUE, name TEXT NOT NULL CHECK(length(trim(name))>0),
 created_at INTEGER NOT NULL, schema_version INTEGER NOT NULL CHECK(schema_version>0), data_keys TEXT NOT NULL CHECK(json_valid(data_keys))
) STRICT;
-- 容器内文件树节点：目录只保存节点关系；文件额外保存密文位置、大小、密钥、随机数、加密时间和版本链。
-- parent_id 指向父目录；根节点 name 为空；文件路径与名字限制在数据库层统一执行，避免不同客户端写出不兼容路径。
CREATE TABLE file_info (
 id TEXT PRIMARY KEY NOT NULL, parent_id TEXT REFERENCES file_info(id), name TEXT NOT NULL,
 kind TEXT NOT NULL CHECK(kind IN ('directory','file')), created_at INTEGER NOT NULL,updated_at INTEGER NOT NULL,
 cipher_name TEXT UNIQUE, size INTEGER CHECK(size>=0),key_id TEXT,nonce BLOB,encrypted_at INTEGER,version_history TEXT,
 CHECK((parent_id IS NULL AND name='' AND kind='directory') OR
       (parent_id IS NOT NULL AND length(name)>0 AND name NOT IN ('.','..') AND instr(name,'/')=0 AND instr(name,char(92))=0 AND instr(name,char(0))=0)),
 CHECK((kind='directory' AND cipher_name IS NULL AND size IS NULL AND key_id IS NULL AND nonce IS NULL AND encrypted_at IS NULL AND version_history IS NULL) OR
       (kind='file' AND cipher_name IS NOT NULL AND size IS NOT NULL AND key_id IS NOT NULL AND nonce IS NOT NULL AND length(nonce)=12 AND encrypted_at IS NOT NULL AND version_history IS NOT NULL AND json_valid(version_history) AND json_type(version_history)='array' AND json_array_length(version_history)>0))
) STRICT;
-- 同一父目录下文件名唯一；BINARY 比较保证大小写不同的名字仍是不同路径。
CREATE UNIQUE INDEX file_siblings ON file_info(parent_id,name COLLATE BINARY);
-- 整个容器只能登记一个根目录节点。
CREATE UNIQUE INDEX file_root ON file_info((1)) WHERE parent_id IS NULL;
-- 新增节点前确认父节点存在且为目录，避免文件被挂到不存在或非目录节点下。
CREATE TRIGGER file_parent_insert BEFORE INSERT ON file_info WHEN NEW.parent_id IS NOT NULL BEGIN
 SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM file_info WHERE id=NEW.parent_id AND kind='directory') THEN RAISE(ABORT,'parent must be directory') END;
END;
-- 修改树关系时阻止根节点移动、文件目录互换导致的非法状态，以及把目录移动进自己的子树形成循环。
CREATE TRIGGER file_parent_update BEFORE UPDATE OF parent_id,kind ON file_info BEGIN
 SELECT CASE WHEN OLD.parent_id IS NULL AND (NEW.parent_id IS NOT NULL OR NEW.kind<>'directory') THEN RAISE(ABORT,'root is immutable') END;
 SELECT CASE WHEN NEW.parent_id IS NOT NULL AND NOT EXISTS(SELECT 1 FROM file_info WHERE id=NEW.parent_id AND kind='directory') THEN RAISE(ABORT,'parent must be directory') END;
 SELECT CASE WHEN NEW.kind='file' AND EXISTS(SELECT 1 FROM file_info WHERE parent_id=NEW.id) THEN RAISE(ABORT,'directory has children') END;
 SELECT CASE WHEN EXISTS(WITH RECURSIVE ancestors(id,parent_id) AS (SELECT id,parent_id FROM file_info WHERE id=NEW.parent_id UNION ALL SELECT f.id,f.parent_id FROM file_info f JOIN ancestors a ON f.id=a.parent_id) SELECT 1 FROM ancestors WHERE id=NEW.id) THEN RAISE(ABORT,'cycle') END;
END;
-- 根目录承载容器文件树身份，禁止单独删除。
CREATE TRIGGER file_root_delete BEFORE DELETE ON file_info WHEN OLD.parent_id IS NULL BEGIN SELECT RAISE(ABORT,'root cannot be deleted'); END;
-- 跨进程文件操作租约：记录被保护的文件、读 / 写模式、所有者凭证和过期时间。
-- 该表用于协调多个客户端对重叠文件/目录/整容器范围的访问；资源类型字段保留 file，实际范围和共享写语义由资源编码前缀解释。
-- owner_identity 是执行者 JSON，owner_endpoint 是可选存活探测地址，owner_token 用于证明当前所有权；expires_at 为 UTC 毫秒到期值，不是本地单调时钟。
CREATE TABLE operation_locks (
 lock_id TEXT PRIMARY KEY NOT NULL,resource_type TEXT NOT NULL CHECK(resource_type='file'), resource_id TEXT NOT NULL,
 mode TEXT NOT NULL CHECK(mode IN ('read','write')),owner_identity TEXT NOT NULL CHECK(json_valid(owner_identity)),
 owner_endpoint TEXT CHECK(owner_endpoint IS NULL OR json_valid(owner_endpoint)),owner_token TEXT NOT NULL,expires_at INTEGER NOT NULL
) STRICT;
-- 查找某资源当前锁和过期租约时按资源标识与截止时间过滤。
CREATE INDEX operation_resource ON operation_locks(resource_type,resource_id,expires_at);
-- 可选的文件内容摘要缓存；file_id 关联文件元数据，删除文件时级联清除摘要。
-- BLAKE3 十六进制摘要用于快速判断内容相同，缺失时服务仍可通过读取内容比较。
CREATE TABLE file_hash (
 file_id TEXT PRIMARY KEY NOT NULL REFERENCES file_info(id) ON DELETE CASCADE,
 blake3 TEXT NOT NULL CHECK(length(blake3)=64 AND blake3 NOT GLOB '*[^0-9a-f]*')
) STRICT;
