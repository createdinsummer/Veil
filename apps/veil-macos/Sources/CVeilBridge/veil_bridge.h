#ifndef VEIL_BRIDGE_H
#define VEIL_BRIDGE_H
/*
 * macOS Swift 应用与 Rust Veil 服务之间的 C ABI 声明。
 * 所属模块：VeilMac 原生桥接层。声明不透明服务句柄、跨边界字节缓冲区以及调用、
 * 进度查询、取消传输和日志接口；Swift 侧通过这些函数把 JSON 请求交给 Rust 服务，
 * 并负责释放由 Rust 返回的缓冲区。业务目标是让 macOS 界面复用 Rust 的容器能力。
 */
#include <stddef.h>
#include <stdint.h>

/* Rust 内部服务对象的句柄；调用方只能持有和传递指针，不能直接访问其内容。 */
typedef struct VeilService VeilService;
/* 跨 Swift/Rust 传递的数据块：data 指向字节内容，length 是字节数，kind 标识结果类型。 */
typedef struct { uint8_t *data; size_t length; uint32_t kind; } VeilBuffer;

/* 创建服务客户端；成功返回句柄，供后续请求和进度查询使用。 */
// 功能：创建服务句柄并初始化日志；入参无；返回本库分配的 VeilService*。
// 异常：无可捕获的 C 错误返回，分配失败或未捕获 Rust panic 可终止进程；注意：不是打开容器，所有并发调用结束后才能 free。
VeilService *veil_client_new(void);
/* 销毁由 veil_client_new 创建的客户端并释放其占用资源。 */
// 功能：归还服务句柄；client 为本库 new 返回且未释放的指针，NULL 无操作；返回 void。
// 异常：非法或重复释放属于未定义行为；注意：释放前须等待所有调用结束，不可把 free 当作安全取消在途请求。
void veil_client_free(VeilService *client);
/* 将 UTF-8/JSON 请求字节交给服务执行，并返回结果缓冲区；返回缓冲区需调用 veil_buffer_free。 */
// 功能：同步执行容器/设置 JSON 请求；client 为有效句柄，data 为稳定可读 UTF-8 JSON 字节，length 为 1 至 1048576。
// 返回：kind 0 JSON、1 错误文本、2 稳定预览字节、3 非稳定预览字节、4 非稳定导出标记；所有返回缓冲均需归还。
// 异常：可检查参数/业务错误及可展开 panic 转 kind 1，非法非空指针无法验证；注意：长任务阻塞，输入内存复制结束前不可改写，句柄不可并发释放。
VeilBuffer veil_client_call(VeilService *client, const uint8_t *data, size_t length);
/* 读取当前客户端的传输进度快照；不会消费或修改正在执行的任务。 */
// 功能：读取客户端共享传输槽的当前快照；client 为存活句柄；返回 kind 0 JSON，NULL 或编码/捕获错误为 kind 1。
// 异常：非法非空地址不是可恢复错误；注意：空闲仍返回状态对象，不代表所有并发任务汇总，内部锁可能等待，缓冲须归还。
VeilBuffer veil_client_progress(const VeilService *client);
/* 请求取消客户端当前传输；返回值表示服务是否接受取消请求。 */
// 功能：请求协作取消当前传输；client 为存活句柄；返回 true 表示首次接受，false 可能为空闲、已请求、NULL 或捕获错误。
// 异常：非法地址不可恢复；注意：可与长请求并发，不强制终止线程，不回滚已提交文件，也不可与 free 并发。
_Bool veil_client_cancel_transfer(const VeilService *client);
/* 把 Swift 侧错误记录交给统一日志系统；category/source/line/native_code/message 描述错误来源。 */
// 功能：提交客户端诊断；category 为 u32 分类码，source 为来源 C 字符串，line 为行号，native_code 为系统数值码，message 为描述。
// 返回 void；source/message 可 NULL，其他指针须可读且 NUL 终止，非 UTF-8 使用占位；可展开 panic 被忽略。
// 注意：调用方须先过滤敏感正文，接口不反馈持久化结果，字符串在调用结束前不可释放或修改。
void veil_log_client_error(uint32_t category, const char *source, uint32_t line, int64_t native_code, const char *message);
/* 尽力刷新日志队列，不返回写入结果。 */
// 功能：请求刷新日志队列；入参无；返回 void，底层错误和可展开 panic 不向调用方报告。
// 注意：同步等待不等于物理磁盘 fsync，也不保证所有历史日志落盘。
void veil_logs_flush(void);
/* 导出日志内容为缓冲区；调用方完成读取后需释放缓冲区。 */
// 功能：读取滚动日志 JSON 供用户导出；入参无；返回 kind 0 日志或 kind 1 错误文本。
// 异常：可展开 panic 转错误缓冲，分配失败不保证恢复；注意：可能等队列和磁盘读取，不重新全面净化历史内容，读取后必须 buffer_free。
VeilBuffer veil_logs_export(void);
/* 释放由桥接层返回的缓冲区及其底层内存。 */
// 功能：清零并释放本库返回的原始载荷；buffer 的 data/length 须保持返回原值，NULL data 无操作；返回 void。
// 异常：伪造地址/长度、重复释放属于未定义行为；注意：只归还一次且不能与读取并发，不清理调用方已复制的 Data 或预览文件。
void veil_buffer_free(VeilBuffer buffer);
#endif
