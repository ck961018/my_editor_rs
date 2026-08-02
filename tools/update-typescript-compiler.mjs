import { copyFile, mkdir, readFile } from "node:fs/promises";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const packageJson = JSON.parse(await readFile(join(root, "package.json"), "utf8"));
const version = packageJson.devDependencies.typescript;
const source = join(root, "node_modules", "typescript");
const destination = join(root, "crates", "vell-plugin-v8", "vendor", "typescript");
const files = [
  ["lib/typescript.js", "typescript.js"],
  ["LICENSE.txt", "LICENSE.txt"],
  ["lib/lib.es5.d.ts", "lib/lib.es5.d.ts"],
  ["lib/lib.es2015.promise.d.ts", "lib/lib.es2015.promise.d.ts"],
  ["lib/lib.decorators.d.ts", "lib/lib.decorators.d.ts"],
  ["lib/lib.decorators.legacy.d.ts", "lib/lib.decorators.legacy.d.ts"],
];

for (const [from, to] of files) {
  const target = join(destination, to);
  await mkdir(dirname(target), { recursive: true });
  await copyFile(join(source, from), target);
}

console.log(`Vendored TypeScript ${version}.`);
