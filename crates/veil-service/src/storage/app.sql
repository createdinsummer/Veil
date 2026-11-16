-- 文件作用：声明本机普通 SQLite 应用库的版本 1 固定结构，供 AppStore 初始化和结构指纹检查。
-- 所属模块：service 本机存储；包含空间 space_info、关联 space_div、用户设置、系统设置和 local_record 五张表及查询索引。
-- 数据边界：只存本机位置、展示缓存、偏好和恢复线索，不能存容器口令或文件内容密钥。
-- 依赖能力：SQLite STRICT 类型、JSON 合法性检查、外键与唯一索引；连接由 core 打开并启用外键，版本/应用标识由 ensure_schema 写入。
-- 业务目标：使不同客户端按同一结构管理本机空间和入口；本文件不单独实现全部字段语义校验、迁移流程或生命周期锁协议。
-- 桌面 / CLI 共用的空间登记表：每行描述一个逻辑存储空间及其本机目录。
-- id 是稳定标识；name / description 用于展示；local_path 指向目录；storage_mode 区分共享或独立空间；排序与时间戳用于界面排序和变更追踪。
CREATE TABLE space_info (
 id TEXT PRIMARY KEY NOT NULL, name TEXT NOT NULL CHECK(length(trim(name))>0), description TEXT,
 local_path TEXT UNIQUE, storage_mode TEXT NOT NULL DEFAULT 'shared' CHECK(storage_mode IN ('shared','dedicated')),
 sort_order INTEGER NOT NULL DEFAULT 0 CHECK(sort_order>=0), created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL
) STRICT;
-- 空间与容器的登记关系：缓存容器名和排序，避免侧栏每次都扫描磁盘推导列表。
CREATE TABLE space_div (
 div_id TEXT PRIMARY KEY NOT NULL, space_id TEXT NOT NULL REFERENCES space_info(id), cached_name TEXT NOT NULL,
 sort_order INTEGER NOT NULL DEFAULT 0 CHECK(sort_order>=0), registered_at INTEGER NOT NULL, updated_at INTEGER NOT NULL
) STRICT;
-- 按空间取其容器时，先按 sort_order 排序，再用 div_id 稳定打破并列。
CREATE INDEX space_div_space ON space_div(space_id,sort_order,div_id);
-- 本机用户偏好；key 是设置项名，value_json 保存布局、浏览方式等可扩展 JSON 值。
CREATE TABLE user_setting (key TEXT PRIMARY KEY NOT NULL,value_json TEXT NOT NULL CHECK(json_valid(value_json)),updated_at INTEGER NOT NULL) STRICT;
-- 本机系统级标识与配置；与用户偏好分表，避免把设备身份当成界面偏好处理。
CREATE TABLE system_setting (key TEXT PRIMARY KEY NOT NULL,value_json TEXT NOT NULL CHECK(json_valid(value_json)),updated_at INTEGER NOT NULL) STRICT;
-- 服务内部的本机业务记录；record_type / record_key 组合定位一条 JSON 数据。
CREATE TABLE local_record (
 record_type TEXT NOT NULL,record_key TEXT NOT NULL,value_json TEXT NOT NULL CHECK(json_valid(value_json)),updated_at INTEGER NOT NULL,
 PRIMARY KEY(record_type,record_key)
) STRICT;
-- 按记录类型读取最近变更时，使用时间倒序和键名稳定排序。
CREATE INDEX local_record_recent ON local_record(record_type,updated_at DESC,record_key);
