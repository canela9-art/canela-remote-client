#!/usr/bin/env python3
"""Convierte canela/privacy-screen.png en el img.cpp de RustDeskTempTopMostWindow (mismo formato
que su herramienta Img2Mem): WindowInjection.dll dibuja ese PNG en el modo privado de Windows.

    python png2imgcpp.py <png> <ruta a WindowInjection/img.cpp>
"""
import sys

png, out = sys.argv[1], sys.argv[2]
data = open(png, "rb").read()
if data[:8] != b"\x89PNG\r\n\x1a\n":
    sys.exit(f"{png} no es un PNG")
lines = [", ".join(f"0x{b:02x}" for b in data[i:i + 20]) + "," for i in range(0, len(data), 20)]
with open(out, "w", newline="\n") as f:
    f.write('#include "pch.h"\n#include "./img.h"\n\nconst unsigned char g_img[] = {\n')
    f.write("\n".join(lines))
    f.write("\n};\n\nconst long long g_imgLen = sizeof(g_img);\n")
print(f"{out}: {len(data)} bytes de {png}")
