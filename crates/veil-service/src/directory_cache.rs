//! 单次文件传输共享的目录缓存，任务结束后释放。
//!
//! 缓存服务于一个文件传输任务（如整目录导入）：
//! - container：容器库中「已经提交」的目录节点快照，避免每次导入都重新
//!   查询目录树；只有对应目录锁覆盖、且事务提交成功后才写入；
//! - filesystem：文件系统上「已确认创建成功」的目录路径，避免任务内部
//!   多个工作线程并发重复创建同一目录。
//! 缓存只对本任务有效，任务结束即随上下文销毁，不跨请求保留。
//! 所属模块：导入/导出任务的目录复用；核心类型 DirectoryCache 和 ensure_filesystem 负责缓存与创建。
//! 依赖能力：容器 Node、进程内 Mutex、HashMap 以及目录创建/同步；目标是减少同任务重复查询和创建，
//! 不是监视文件系统变化的服务。container 为公开内部映射，其“提交后写入”约束由调用方执行。
use crate::{
    error::Result,
    storage::{container::Node, invalid},
};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Mutex,
};

/// 单任务多工作线程共用的目录缓存，减少导入节点查找与导出目录创建。
/// container 的键为容器逻辑路径、值为已提交目录 Node；filesystem 的键为原样本地路径、值为空占位。
/// 两个映射各自一把 Mutex，容器缓存的租约覆盖和提交顺序由调用方保证；不对外部变更自动失效。
#[derive(Default)]
pub(crate) struct DirectoryCache {
    /// 仅缓存目录锁覆盖的容器节点；事务提交成功后才能写入。
    pub container: Mutex<HashMap<String, Node>>,
    // 文件系统目录缓存：键是调用方传入的已确认存在的路径及其祖先，值是空占位；
    // 不主动转换为绝对路径，不同写法的同一目录可能分别占用缓存项。
    // 任务内多线程共用一把互斥锁串行化「检查-创建-记录」。
    filesystem: Mutex<HashMap<PathBuf, ()>>,
}
impl DirectoryCache {
    /// 确保一个本地输出目录已建立，并在本任务内复用检查结果。
    /// 入参：self（&DirectoryCache）为任务共享缓存；path（&Path）为可相对或绝对的目录路径，保留调用方原写法。
    /// 返回值：命中缓存或创建/确认目录并完成同步请求后返回 Ok(())。
    /// 异常：缓存 Mutex 中毒、目标类型无效、目录创建或同步失败返回 VeilError。
    /// 注意事项：持 filesystem 锁覆盖查缓存、磁盘创建和插入，其他工作线程会等待；失败不记缓存，但已创建目录可能保留。命中后不重新查磁盘，外部删除/替换不会自动失效。
    pub fn ensure_filesystem(&self, path: &Path) -> Result<()> {
        // 先查缓存：本次任务已经确认存在的目录直接放行，不再碰文件系统。
        let mut known = self
            .filesystem
            .lock()
            .map_err(|_| invalid("目录缓存不可用"))?;
        if known.contains_key(path) {
            return Ok(());
        }
        // 缓存未命中才创建目录，并请求同步新目录项对应的父目录。
        crate::fsutil::create_dir_all_durable(path)?;
        // 逐层记录目标及非空祖先；每一项代表本次创建成功后可复用的目录路径。
        for ancestor in path.ancestors().filter(|p| !p.as_os_str().is_empty()) {
            known.insert(ancestor.to_owned(), ());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    /// 验证多个工作线程并发确保同一目录时只共享一次成功创建结果。
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn concurrent_threads_share_created_directories() {
        let root = tempfile::tempdir().unwrap();
        let cache = DirectoryCache::default();
        std::thread::scope(|scope| {
            for i in 0..32 {
                let cache = &cache;
                let path = root.path().join(format!("shared/nested/{}", i % 4));
                scope.spawn(move || cache.ensure_filesystem(&path).unwrap());
            }
        });
        let known = cache.filesystem.lock().unwrap();
        for i in 0..4 {
            let path = root.path().join(format!("shared/nested/{i}"));
            assert!(path.is_dir());
            assert!(known.contains_key(&path));
        }
        assert!(known.contains_key(&root.path().join("shared")));
    }
    #[test]
    /// 验证目录创建失败不会写入缓存，后续调用仍可重试。
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn failed_creation_is_not_cached_and_can_be_retried() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("blocked");
        let cache = DirectoryCache::default();
        std::fs::write(&path, b"keep").unwrap();
        assert!(cache.ensure_filesystem(&path).is_err());
        assert!(!cache.filesystem.lock().unwrap().contains_key(&path));
        assert_eq!(std::fs::read(&path).unwrap(), b"keep");
        std::fs::remove_file(&path).unwrap();
        cache.ensure_filesystem(&path).unwrap();
        assert!(path.is_dir());
    }
    #[test]
    /// 验证回滚产生的未提交目录不会进入共享缓存，工作线程和批次间只共享已提交目录。
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn container_cache_ignores_rollback_and_is_shared_between_workers_and_batches() {
        use crate::veil_ops::{AddFileSpec, ImportBatchContext, VeilManager};
        use std::sync::{Arc, atomic::AtomicBool};
        veil_core::kdf::enable_fast_test_kdf();
        let root = tempfile::tempdir().unwrap();
        let manager = VeilManager::new(root.path().join("container"));
        manager.init_veil("test", "password").unwrap();
        let (guard, scopes) = manager
            .lock_import_directories(
                &["album".into()],
                "password",
                Arc::new(AtomicBool::new(false)),
            )
            .unwrap();
        let guard = Arc::new(guard);
        let directories = Arc::new(DirectoryCache::default());
        let (sender, receiver) = veil_core::queue::BoundedQueue::new(16).split();
        let owner = crate::ExecutionService::global()
            .unwrap()
            .runtime()
            .current_context()
            .owner()
            .clone();
        let context = ImportBatchContext {
            sender: sender.clone(),
            directory_guard: Some(guard.clone()),
            directories: directories.clone(),
            owner: owner.clone(),
        };
        let gate = Mutex::new(());
        let source = root.path().join("source");
        std::fs::write(&source, b"content").unwrap();
        let failed = [
            AddFileSpec::new(&source, "album/shared/deep/first"),
            AddFileSpec::new(root.path().join("missing"), "album/shared/deep/second"),
        ];
        assert!(
            manager
                .add_files_for_import(
                    &failed,
                    "password",
                    Some((&guard, &scopes[0])),
                    &gate,
                    &context
                )
                .is_err()
        );
        assert!(directories.container.lock().unwrap().is_empty());
        assert!(
            manager
                .open_db("password")
                .unwrap()
                .resolve("album/shared")
                .unwrap()
                .is_none()
        );
        std::thread::scope(|scope| {
            for worker in 0..4 {
                let context = ImportBatchContext {
                    sender: sender.clone(),
                    directory_guard: Some(guard.clone()),
                    directories: directories.clone(),
                    owner: owner.clone(),
                };
                let manager = &manager;
                let guard = &guard;
                let directory = &scopes[0];
                let gate = &gate;
                let source = &source;
                scope.spawn(move || {
                    for batch in 0..2 {
                        manager
                            .add_files_for_import(
                                &[AddFileSpec::new(
                                    source,
                                    format!("album/shared/deep/{worker}-{batch}"),
                                )],
                                "password",
                                Some((guard, directory)),
                                gate,
                                &context,
                            )
                            .unwrap();
                    }
                });
            }
        });
        let items = receiver.try_recv_batch(16).unwrap();
        assert_eq!(items.len(), 8);
        manager
            .commit_import_batch(items, "password", &AtomicBool::new(false))
            .unwrap();
        let known = directories.container.lock().unwrap();
        assert_eq!(known.len(), 4);
        let node = manager
            .open_db("password")
            .unwrap()
            .resolve("album/shared/deep")
            .unwrap()
            .unwrap();
        assert_eq!(known["album/shared/deep"].id, node.id);
        assert_eq!(
            manager
                .open_db("password")
                .unwrap()
                .children(&node.id, 0, 100)
                .unwrap()
                .len(),
            8
        );
    }
}
