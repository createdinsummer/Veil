import { readFileSync, writeFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { resolve, dirname } from 'node:path';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const check = process.argv.includes('--check');
const tokens = JSON.parse(readFileSync(resolve(root, 'tokens.json'), 'utf8'));
const flatten = (value, prefix = '') => Object.entries(value).flatMap(([key, item]) => {
  const name = prefix ? `${prefix}-${key}` : key;
  return typeof item === 'object' ? flatten(item, name) : [`  --${name}: ${item};`];
});
const base = Object.entries(tokens).filter(([key]) => !['name', 'version', 'color'].includes(key));
const css = `/* 自动生成：编辑 ui/tokens.json 后运行 node ui/tools/build.mjs。 */\n:root {\n${[
  ...flatten(tokens.color.light, 'color'), ...base.flatMap(([key, value]) => flatten(value, key))
].join('\n')}\n  color-scheme: light;\n}\n[data-theme="dark"] {\n${flatten(tokens.color.dark, 'color').join('\n')}\n  color-scheme: dark;\n}\n`;
const sprite = readFileSync(resolve(root, 'assets/icons.svg'), 'utf8');
const icons = Object.fromEntries([...sprite.matchAll(/<symbol id="([^"]+)"[^>]*>([\s\S]*?)<\/symbol>/g)].map(([, id, body]) => [id, body]));
if (!icons.folder || !icons.lock) throw new Error('图标素材不完整');
const script = `/* 自动生成自 ui/assets/icons.svg，支持直接以本地文件打开原型。 */\nwindow.VeilIcons = ${JSON.stringify(icons, null, 2)};\n`;
const tokenScript = `/* 自动生成自 ui/tokens.json。 */\nwindow.VeilDesignTokens = ${JSON.stringify(tokens, null, 2)};\n`;
for (const [path, content] of [['foundations/tokens.css', css], ['foundations/icons.js', script], ['foundations/tokens.js', tokenScript]]) {
  const target = resolve(root, path);
  if (check) {
    if (readFileSync(target, 'utf8') !== content) throw new Error(`${path} 与设计源文件不同步`);
  } else { writeFileSync(target, content); }
}
console.log(check ? '设计参数与图标检查通过。' : '已生成设计样式与图标。');
