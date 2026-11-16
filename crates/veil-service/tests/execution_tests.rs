//! 通过真实子进程验证查询协议，不依赖对其他进程线程的系统查询权限。
//!
//! 所属模块：veil-service 执行生命周期集成测试。execution_child 在子进程登记
//! 工作线程和任务，Advertisement 把端点及两类所有者身份发给父进程；其余测试
//! 通过请求消息核对存活状态及查询权限边界，ChildGuard 在析构时尝试终止并等待子进程，系统调用错误会被忽略。
//! 依赖能力：serde JSON 序列化、标准输入输出管道、mpsc 同步通道与 veil-service 查询协议。
//! 业务目标：确认跨进程的任务监测能区分仍在运行、已结束和不可查询的对象。

use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;
use veil_service::execution::{ExecutionEndpoint, ExecutionOwner, ExecutionService, Liveness};
use veil_service::{ServiceRequest, ServiceResponse, VeilService};

/// 子进程向父测试进程公布的执行服务端点和存活对象身份。
#[derive(Serialize, Deserialize)]
struct Advertisement {
    /// 子进程监听的本地查询端点。
    endpoint: ExecutionEndpoint,
    /// 被监测工作线程的唯一所有者身份。
    thread: ExecutionOwner,
    /// 工作线程内登记任务的唯一所有者身份。
    task: ExecutionOwner,
}

#[test]
/// 在子进程中启动可观测的后台线程 / 任务，供生命周期查询接口读取。
/// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
/// 返回值：无；正常执行至末尾表示本场景通过。
/// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
/// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
fn execution_child() {
    if std::env::var_os("VEIL_EXECUTION_CHILD").is_none() {
        return;
    }
    let service = ExecutionService::global().unwrap();
    let server = service.listen().unwrap();
    let (ready_tx, ready_rx) = mpsc::channel();
    let (stop_tx, stop_rx) = mpsc::channel();
    let worker_service = service.clone();
    let worker = service
        .spawn_thread("子进程工作线程", move |context| {
            let task = worker_service.enter_task("子进程任务");
            ready_tx
                .send((context.thread, task.owner().clone()))
                .unwrap();
            stop_rx.recv_timeout(Duration::from_secs(15)).unwrap();
        })
        .unwrap();
    let (thread, task) = ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let advertisement = Advertisement {
        endpoint: server.endpoint().clone(),
        thread,
        task,
    };
    println!(
        "VEIL_EXECUTION_READY={}",
        serde_json::to_string(&advertisement).unwrap()
    );
    std::io::stdout().flush().unwrap();
    let mut worker = Some(worker.handle);
    for line in std::io::stdin().lock().lines() {
        match line.unwrap().as_str() {
            "finish" => {
                stop_tx.send(()).unwrap();
                worker.take().unwrap().join().unwrap();
                println!("VEIL_EXECUTION_FINISHED");
                std::io::stdout().flush().unwrap();
            }
            "exit" => break,
            other => panic!("未知测试命令: {other}"),
        }
    }
    if let Some(worker) = worker {
        let _ = stop_tx.send(());
        worker.join().unwrap();
    }
}

/// 确保测试结束时回收外部子进程，避免测试残留后台进程。
/// 元组字段保存当前测试启动的子进程句柄，析构时先终止再等待退出。
struct ChildGuard(Child);

impl Drop for ChildGuard {
    /// 在测试守卫释放时回收其持有的子进程，减少测试失败后的后台残留。
    /// 入参：self（&mut ChildGuard）独占当前子进程句柄。
    /// 返回值：()；依次尝试终止并等待子进程。
    /// 异常：kill 和 wait 的错误均被忽略，不作为析构 panic 传播。
    /// 注意事项：wait 没有超时，仍可能阻塞；此处不发送业务停止通知，也不保证回收子进程派生的其他进程。
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// 等待子进程输出指定阶段标记，并提取标记后的业务结果文本。
/// 入参：reader（&mut impl BufRead）为子进程输出读取器；prefix（&str）为待查找标记，可出现在一行任意位置，空串会匹配首行。
/// 返回值：String，首个包含标记的行中，标记后内容去除首尾空白的副本；此前行被消费。
/// 异常：读取失败或匹配前到达 EOF 会 panic，使测试失败。
/// 注意事项：按子串而非行首匹配，没有超时和行长限制；调用方负责子进程会输出标记且不会无限等待。
fn read_until(reader: &mut impl BufRead, prefix: &str) -> String {
    loop {
        let mut line = String::new();
        assert_ne!(
            reader.read_line(&mut line).unwrap(),
            0,
            "子进程未返回预期消息: {prefix}"
        );
        if let Some(index) = line.find(prefix) {
            return line[index + prefix.len()..].trim().to_owned();
        }
    }
}

#[test]
/// 验证线程和任务查询结果会随真实启动、运行和结束状态变化。
/// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
/// 返回值：无；正常执行至末尾表示本场景通过。
/// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
/// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
fn remote_thread_and_task_queries_follow_real_lifecycle() {
    let mut child = ChildGuard(
        Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "execution_child", "--nocapture"])
            .env("VEIL_EXECUTION_CHILD", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    );
    let mut output = BufReader::new(child.0.stdout.take().unwrap());
    let advertisement: Advertisement =
        serde_json::from_str(&read_until(&mut output, "VEIL_EXECUTION_READY=")).unwrap();
    let service = ExecutionService::global().unwrap();
    let endpoint = &advertisement.endpoint;
    let timeout = Duration::from_secs(2);
    assert_eq!(
        service.check_owner(&advertisement.thread, Some(endpoint), timeout),
        Liveness::Alive
    );
    assert_eq!(
        service.check_owner(&advertisement.task, Some(endpoint), timeout),
        Liveness::Alive
    );
    assert_eq!(
        service.check_owner(&advertisement.thread, None, timeout),
        Liveness::Unknown
    );
    let unknown = ExecutionOwner::Task {
        process: endpoint.process.clone(),
        task_id: "unknown-task".into(),
    };
    assert_eq!(
        service.check_owner(&unknown, Some(endpoint), timeout),
        Liveness::Unknown
    );
    let mut wrong_key = serde_json::to_value(endpoint).unwrap();
    wrong_key["capability"] = "wrong".into();
    let wrong_key: ExecutionEndpoint = serde_json::from_value(wrong_key).unwrap();
    assert_eq!(
        service.check_owner(&advertisement.task, Some(&wrong_key), timeout),
        Liveness::Unknown
    );
    match VeilService::default()
        .execute(ServiceRequest::CheckExecution {
            owner: &advertisement.task,
            endpoint: Some(endpoint),
            timeout,
        })
        .unwrap()
    {
        ServiceResponse::ExecutionStatus(status) => assert_eq!(status, Liveness::Alive),
        _ => panic!("响应类型错误"),
    }
    child
        .0
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"finish\n")
        .unwrap();
    read_until(&mut output, "VEIL_EXECUTION_FINISHED");
    assert_eq!(
        service.check_owner(&advertisement.thread, Some(endpoint), timeout),
        Liveness::Dead
    );
    assert_eq!(
        service.check_owner(&advertisement.task, Some(endpoint), timeout),
        Liveness::Dead
    );
    assert_ne!(service.check_process(&endpoint.process), Liveness::Dead);
    child.0.kill().unwrap();
    child.0.wait().unwrap();
    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    assert_eq!(service.check_process(&endpoint.process), Liveness::Dead);
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    assert_eq!(
        service.check_owner(&advertisement.task, Some(endpoint), timeout),
        Liveness::Unknown
    );
}

#[test]
/// 验证已登记的工作可由调度器接收，不依赖特定运行时环境。
/// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
/// 返回值：无；正常执行至末尾表示本场景通过。
/// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
/// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
fn schedulers_receive_registered_work_without_runtime_dependency() {
    let service = ExecutionService::global().unwrap();
    let job = service.spawn_blocking(
        "线程池任务",
        |context| context.owner().clone(),
        std::thread::spawn,
    );
    assert_eq!(job.handle.join().unwrap(), job.owner);
    assert_eq!(service.check_local(&job.owner), Liveness::Dead);
    let task = service.spawn_async("异步任务", std::future::pending::<()>(), |future| {
        future
    });
    assert_eq!(service.check_local(&task.owner), Liveness::Alive);
    drop(task.handle);
    assert_eq!(service.check_local(&task.owner), Liveness::Dead);
}
