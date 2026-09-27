#!/usr/bin/env python3
import base64
import gzip
import hashlib
import pathlib
import urllib.request

WEB = pathlib.Path(__file__).resolve().parent
WASM = WEB.parent / "target/wasm32-unknown-unknown/web/dm_web.wasm"
LEAFLET = {
    "leaflet.js": "db49d009c841f5ca34a888c96511ae936fd9f5533e90d8b2c4d57596f4e5641a",
    "leaflet.css": "a7837102824184820dfa198d1ebcd109ff6d0ff9a2672a074b9a1b4d147d04c6",
}


def leaflet(name: str) -> str:
    cached = WEB / ".cache" / name
    if not cached.exists():
        cached.parent.mkdir(exist_ok=True)
        cached.write_bytes(urllib.request.urlopen(f"https://cdnjs.cloudflare.com/ajax/libs/leaflet/1.9.4/{name}").read())
    data = cached.read_bytes()
    if hashlib.sha256(data).hexdigest() != LEAFLET[name]:
        raise SystemExit(f"{name} does not match its pinned checksum")
    return data.decode()


wasm = WASM.read_bytes()
packed = gzip.compress(wasm, compresslevel=9, mtime=0)
page = (
    (WEB / "demo.html")
    .read_text()
    .replace("/*LEAFLET_CSS*/", leaflet("leaflet.css"))
    .replace("/*LEAFLET_JS*/", leaflet("leaflet.js"))
    .replace("__WASM_GZIP_BASE64__", base64.b64encode(packed).decode())
)
target = WEB / "dist" / "matrix-demo.html"
target.parent.mkdir(exist_ok=True)
target.write_text(page)
print(f"{target.relative_to(WEB.parent)}: {len(page) / 1e3:.0f} kB (WebAssembly {len(wasm) / 1e3:.0f} kB, {len(packed) / 1e3:.0f} kB gzipped)")
