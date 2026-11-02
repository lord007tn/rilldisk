#!/bin/bash
# SPDX-License-Identifier: AGPL-3.0-only
# Build the synthetic NTFS image behind crates/rilldisk-core/tests/ntfs_image.rs, or a
# large one for MFT benchmarks, without Windows administrator rights. Linux (or WSL)
# as root, with ntfs-3g installed (mkntfs, ntfs-3g). Nothing here comes from a real disk.
#
#   sudo scripts/make_ntfs_image.sh tiny OUT.img          # the 8 MiB test image (gzip it)
#   sudo scripts/make_ntfs_image.sh fixture OUT.img N SIZE MAKE_FIXTURE
#        # SIZE-byte volume holding `make_fixture OUT/fx --files N` (manifest next to OUT.img)
set -euo pipefail
kind=$1
img=$2
mount=$(mktemp -d)
rm -f "$img"
if [ "$kind" = tiny ]; then truncate -s 8M "$img"; else truncate -s "$4" "$img"; fi
mkntfs -F -Q -c 4096 -L RILLDISK "$img" >/dev/null 2>&1
ntfs-3g "$img" "$mount"
trap 'umount "$mount"; rmdir "$mount"' EXIT
if [ "$kind" = tiny ]; then
  cd "$mount"
  mkdir -p a/b/c many trail "ünïcødé-مرحبا" links
  yes rilldisk | head -c 17 > a/small.txt
  yes rilldisk | head -c 100000 > a/big.bin
  ln a/big.bin a/b/link.bin
  yes rilldisk | head -c 5000 > a/b/c/deep.bin
  yes rilldisk | head -c 1234 > "ünïcødé-مرحبا/名前.txt"
  truncate -s 10M sparse.bin
  for i in $(seq 1 300); do printf '%0*d' "$i" 0 > "many/f$i.txt"; done
  # 51 names of one file overflow its MFT record into extension records.
  yes rilldisk | head -c 9000 > many-links-target.bin
  for i in $(seq 1 50); do ln many-links-target.bin "links/link-$i-$(printf 'n%.0s' $(seq 1 60)).bin"; done
  head -c 42 /dev/zero > "$(printf 'L%.0s' $(seq 1 200)).txt"
  cd /
else
  "$5" "$mount/fx" --files "$3" > "${img%.img}.manifest.json"
fi
sync
