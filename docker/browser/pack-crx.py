#!/usr/bin/env python3
import hashlib
import io
import os
import struct
import subprocess
import sys
import zipfile

def varint(n: int) -> bytes:
    out = bytearray()
    while True:
        b = n & 0x7F
        n >>= 7
        out.append(b | (0x80 if n else 0))
        if not n:
            return bytes(out)

def field(number: int, data: bytes) -> bytes:
    return varint((number << 3) | 2) + varint(len(data)) + data

def main(src: str, key: str, out: str) -> None:
    buf = io.BytesIO()
    with zipfile.ZipFile(buf, "w", zipfile.ZIP_DEFLATED) as z:
        for root, _, files in sorted(os.walk(src)):
            for name in sorted(files):
                path = os.path.join(root, name)
                info = zipfile.ZipInfo(os.path.relpath(path, src), date_time=(2020, 1, 1, 0, 0, 0))
                info.compress_type = zipfile.ZIP_DEFLATED
                with open(path, "rb") as f:
                    z.writestr(info, f.read())
    archive = buf.getvalue()

    public_der = subprocess.run(
        ["openssl", "pkey", "-in", key, "-pubout", "-outform", "DER"], check=True, capture_output=True
    ).stdout
    crx_id = hashlib.sha256(public_der).digest()[:16]
    signed_data = field(1, crx_id)
    to_sign = b"CRX3 SignedData\x00" + struct.pack("<I", len(signed_data)) + signed_data + archive
    signature = subprocess.run(
        ["openssl", "dgst", "-sha256", "-sign", key], input=to_sign, check=True, capture_output=True
    ).stdout
    proof = field(1, public_der) + field(2, signature)
    header = field(2, proof) + field(10000, signed_data)
    with open(out, "wb") as f:
        f.write(b"Cr24" + struct.pack("<I", 3) + struct.pack("<I", len(header)) + header + archive)
    print("".join(chr(ord("a") + int(c, 16)) for c in crx_id.hex()))

if __name__ == "__main__":
    if len(sys.argv) != 4:
        sys.exit("usage: pack-crx.py SRC_DIR KEY_PEM OUT_CRX")
    main(*sys.argv[1:])
