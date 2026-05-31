// Rasterize the dbopt brand mark (web/public/logo.svg) into the OS-level
// application icons that ship inside every installer:
//   - crates/backend/wix/dbopt.ico  -> embedded in dbopt.exe + Windows MSI shortcut/ARP
//   - assets/dbopt.icns             -> macOS dbopt.app/Contents/Resources
//   - assets/dbopt-256.png          -> Linux AppImage icon (was an empty `touch`ed file)
//   - assets/icons/icon-*.png       -> individual PNGs (also feed the web favicon set)
//
// The logo is a full-bleed rounded square (its own background), so it needs no
// extra padding — we render the 48-unit viewBox at high density, then resize.
import sharp from "sharp";
import pngToIco from "png-to-ico";
import { Icns, IcnsImage } from "@fiahfy/icns";
import { readFileSync, writeFileSync, mkdirSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const here = dirname(fileURLToPath(import.meta.url));
const repo = join(here, "..", "..");
const svg = readFileSync(join(repo, "web", "public", "logo.svg"));

const outIcons = join(repo, "assets", "icons");
mkdirSync(outIcons, { recursive: true });
mkdirSync(join(repo, "assets"), { recursive: true });
mkdirSync(join(repo, "crates", "backend", "wix"), { recursive: true });

// Render a crisp square PNG of side `size` from the SVG.
async function png(size) {
  return sharp(svg, { density: 384 })
    .resize(size, size, { fit: "contain", background: { r: 0, g: 0, b: 0, alpha: 0 } })
    .png()
    .toBuffer();
}

const ICO_SIZES = [16, 24, 32, 48, 64, 128, 256];
const ICNS_SIZES = [16, 32, 64, 128, 256, 512, 1024];
const ALL = [...new Set([...ICO_SIZES, ...ICNS_SIZES])].sort((a, b) => a - b);

const buf = {};
for (const s of ALL) {
  buf[s] = await png(s);
  writeFileSync(join(outIcons, `icon-${s}.png`), buf[s]);
}

// Windows .ico (multi-resolution) — used by winresource (exe) + WiX (shortcut/ARP).
const ico = await pngToIco(ICO_SIZES.map((s) => buf[s]));
writeFileSync(join(repo, "crates", "backend", "wix", "dbopt.ico"), ico);

// Linux AppImage icon (256px PNG).
writeFileSync(join(repo, "assets", "dbopt-256.png"), buf[256]);

// macOS .icns. Each size maps to its modern OSType (retina @2x variants included).
const OSTYPE = {
  16: "icp4", 32: "icp5", 64: "icp6",
  128: "ic07", 256: "ic08", 512: "ic09", 1024: "ic10",
};
const icns = new Icns();
for (const s of ICNS_SIZES) {
  icns.append(IcnsImage.fromPNG(buf[s], OSTYPE[s]));
}
writeFileSync(join(repo, "assets", "dbopt.icns"), icns.data);

console.log("icons written:");
console.log("  crates/backend/wix/dbopt.ico  (" + ico.length + " bytes, " + ICO_SIZES.join("/") + ")");
console.log("  assets/dbopt.icns             (" + icns.data.length + " bytes, " + ICNS_SIZES.join("/") + ")");
console.log("  assets/dbopt-256.png          (" + buf[256].length + " bytes)");
console.log("  assets/icons/icon-*.png       (" + ALL.length + " sizes)");
