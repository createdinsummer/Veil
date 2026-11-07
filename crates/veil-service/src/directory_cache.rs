//! 单次文件传输共享的目录缓存，任务结束后释放。
use crate::{
    error::Result,
    storage::{container::Node, invalid},
};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Mutex,
};

#[derive(Default)]
pub(crate) struct DirectoryCache {
    /// 仅缓存目录锁覆盖的容器节点；事务提交成功后才能写入。
    pub container: Mutex<HashMap<String, Node>>,
    filesystem: Mutex<HashMap<PathBuf, ()>>,
}
impl DirectoryCache {
    /// 检查缓存、创建目录和记录成功结果串行执行，避免任务内多个线程重复创建。
    /// 缓存只对本任务有效；外部删除目录时，后续文件操作仍会报错。
    pub fn ensure_filesystem(&self, path: &Path) -> Result<()> {
        let mut known = self
            .filesystem
            .lock()
            .map_err(|_| invalid("目录缓存不可用"))?;
        if known.contains_key(path) {
            return Ok(());
        }
        crate::fsutil::create_dir_all_durable(path)?;
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
