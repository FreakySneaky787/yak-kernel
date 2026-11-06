"""Makes the test disk of chapter 16: 1 MiB, 2048 sectors. Sector n starts with the line
"YAK test disk, sector n" and is filled up with the byte n % 251 - so a test can tell every sector from its
neighbours, at its start and at its end.

    python3 -I mkdisk.py build/disk.img
"""

import sys

SECTOR_SIZE = 512
SECTORS = 2048


def sector(n):
    text = f"YAK test disk, sector {n}\n".encode()
    return text + bytes([n % 251]) * (SECTOR_SIZE - len(text))


with open(sys.argv[1], "wb") as disk:
    for n in range(SECTORS):
        disk.write(sector(n))
