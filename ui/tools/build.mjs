/*
 * Web 原型设计资源构建脚本。
 * 所属模块：ui 设计基础设施；从 tokens.json 和 icons.svg 生成 CSS 变量与浏览器图标映射。
 * 核心数据：root 是 ui 资源目录；tokens 是共享设计参数；icons 是 SVG symbol 名称到内容的映射；
 * css、script、tokenScript 是生成结果，check 开关只比较文件而不写入。
 * 核心步骤：递归展开嵌套 token、提取图标 symbol、校验必需图标，再写入或校验三个生成文件。
 * 依赖能力：Node.js 文件系统、URL 路径解析和 SVG / JSON 文本处理。
 * 整体目标：让 Web 原型和原生客户端使用同一套颜色、尺寸及图标源，避免手动复制后内容漂移。
 */
import { readFileSync, writeFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { resolve, dirname } from 'node:path';

// 所有路径均相对 ui 目录计算，脚本从任意当前目录执行时都指向同一资源。
const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
// 参数数组中精确出现 --check 时进入只读模式，其他参数忽略；源读取或 JSON 解析错误直接向顶层传播。
const check = process.argv.includes('--check');
// 设计 token 是人工维护的唯一来源；读成对象后递归生成 CSS 与浏览器数据脚本。
const tokens = JSON.parse(readFileSync(resolve(root, 'tokens.json'), 'utf8'));
/**
 * 将一组嵌套设计参数展开为 CSS 变量声明，供明暗主题复用。
 * @param {object} value 当前参数组；叶子值应是可直接写入 CSS 的数值或字符串。
 * @param {string} prefix 当前属性名前缀；递归时用连字符连接各层键名。
 * @returns {string[]} 变量声明行；每一项对应一个叶子参数，调用方负责换行拼接。
 * @throws {TypeError} 输入不是可枚举对象等情况下由 Object.entries 抛出。
 * @remarks 数据来自可信的 tokens.json，嵌套对象不能含循环引用或 null。
 */
const flatten = (value, prefix = '') => Object.entries(value).flatMap(([key, item]) => {
  // 每轮一项属性：key 是本层参数名，item 是子分组或叶子值；flatMap 把递归得到的声明数组拼成同一层列表。
  // 路径各段拼接为变量名，叶子参数通过模板字符串写入；不校验单位、CSS 语法或特殊字符。
  const name = prefix ? `${prefix}-${key}` : key;
  // 数组也会按索引展开；null 被视为 object 后传入 Object.entries 会抛错，调用方必须维护可信源数据。
  return typeof item === 'object' ? flatten(item, name) : [`  --${name}: ${item};`];
});
// 除名称、版本和分主题颜色外，字体、间距、圆角等设计项放入两个主题共用根变量。
const base = Object.entries(tokens).filter(([key]) => !['name', 'version', 'color'].includes(key));
const css = `/*\n * 自动生成的 Web 设计变量。\n * 所属模块：UI 基础样式；定义明暗主题颜色及字体、间距、圆角、布局、动画和阴影变量。\n * 唯一来源是 ui/tokens.json；请编辑源 JSON 后运行 node ui/tools/build.mjs 重新生成本文件。\n */\n:root {\n${[
  ...flatten(tokens.color.light, 'color'), ...base.flatMap(([key, value]) => flatten(value, key))
].join('\n')}\n  color-scheme: light;\n}\n[data-theme="dark"] {\n${flatten(tokens.color.dark, 'color').join('\n')}\n  color-scheme: dark;\n}\n`;
// SVG symbol 源文件承载图标路径；生成的浏览器映射只保留 id 和 symbol 内部图形。
const sprite = readFileSync(resolve(root, 'assets/icons.svg'), 'utf8');
// 每个正则匹配对应一个 id 属性在标签开头的 symbol，提取名称和内部 SVG 文本；不是完整 XML 解析器。
// 同名 id 在 Object.fromEntries 中由最后一项覆盖，其他 symbol 属性不保留，输入必须符合项目素材约定。
const icons = Object.fromEntries([...sprite.matchAll(/<symbol id="([^"]+)"[^>]*>([\s\S]*?)<\/symbol>/g)].map(([, id, body]) => [id, body]));
// 文件夹和锁是界面必需图标；缺失时停止生成，避免交付明显不完整的素材。
if (!icons.folder || !icons.lock) throw new Error('图标素材不完整');
const script = `/*
 * Veil Web 原型图标表（自动生成）。
 * 所属模块：UI 设计基础层，供桌面原型和移动原型通过图标键取 SVG 图形。
 * 数据结构：window.VeilIcons 是 Map 风格的普通对象，键为图标名称，值为 SVG path / shape 片段。
 * 依赖能力：浏览器全局 window 对象与内联 SVG 渲染。
 * 整体目标：统一界面图标来源；请在 ui/assets/icons.svg 修改图形，再运行 ui/tools/build.mjs 重新生成。
 */\nwindow.VeilIcons = ${JSON.stringify(icons, null, 2)};\n`;
const tokenScript = `/*
 * Veil Web 原型设计 token（自动生成）。
 * 所属模块：UI 设计基础层，为颜色主题、字体、间距、圆角、布局、动画和阴影提供一致参数。
 * 数据结构：window.VeilDesignTokens 按 color / font / space / radius / layout / motion / shadow 分组；
 * color 下再按 light / dark 主题提供颜色值，其余分组供多个页面共享。
 * 依赖能力：浏览器全局 window 对象和 ui/tokens.json 源文件。
 * 整体目标：避免组件各自硬编码样式；请编辑 ui/tokens.json 后运行 ui/tools/build.mjs 生成本文件。
 */\nwindow.VeilDesignTokens = ${JSON.stringify(tokens, null, 2)};\n`;
// 输出列表每项为 [相对路径, 完整文本]：CSS 变量、图标映射和原始 token 数据，按此顺序处理。
// 校验要求目标已存在且文本完全相同，缺失会直接抛文件错误；普通模式不先比较，始终覆盖，且不创建父目录。
// 写入不是原子替换，三个文件也不组成事务；中途失败可留下部分新文件，脚本未提供并发锁。
for (const [path, content] of [['foundations/tokens.css', css], ['foundations/icons.js', script], ['foundations/tokens.js', tokenScript]]) {
  // 当前迭代仅处理一个生成物，不会同时验证或写入其他目标。
  const target = resolve(root, path);
  if (check) {
    if (readFileSync(target, 'utf8') !== content) throw new Error(`${path} 与设计源文件不同步`);
  } else { writeFileSync(target, content); }
}
console.log(check ? '设计参数与图标检查通过。' : '已生成设计样式与图标。');
