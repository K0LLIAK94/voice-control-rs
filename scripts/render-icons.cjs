/* Optional asset-development tool. The shipped Rust application needs no Node.
 * npm install --prefix target/icon-tools sharp
 * Set NODE_PATH to target/icon-tools/node_modules, then run this file with Node.
 */
const fs = require('node:fs/promises');
const path = require('node:path');
const sharp = require('sharp');
const assets = path.resolve(__dirname, '..', 'assets');
const sizes = [16, 20, 24, 32, 40, 48, 64, 96, 128, 256];

async function render(name) {
  const svg = await fs.readFile(path.join(assets, `${name}.svg`));
  const images = await Promise.all(sizes.map(size => sharp(svg).resize(size, size).png().toBuffer()));
  const directory = Buffer.alloc(6 + 16 * images.length);
  directory.writeUInt16LE(1, 2);
  directory.writeUInt16LE(images.length, 4);
  let offset = directory.length;
  images.forEach((image, index) => {
    const entry = 6 + index * 16;
    directory[entry] = sizes[index] === 256 ? 0 : sizes[index];
    directory[entry + 1] = directory[entry];
    directory.writeUInt16LE(1, entry + 4);
    directory.writeUInt16LE(32, entry + 6);
    directory.writeUInt32LE(image.length, entry + 8);
    directory.writeUInt32LE(offset, entry + 12);
    offset += image.length;
  });
  await fs.writeFile(path.join(assets, `${name}.ico`), Buffer.concat([directory, ...images]));
  await fs.writeFile(path.join(assets, `${name}.png`), images.at(-1));
}

Promise.all(['icon-idle', 'icon-speaking'].map(render)).catch(error => { console.error(error); process.exitCode = 1; });
