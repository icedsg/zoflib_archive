#!/bin/bash
cd ~/zoflab || exit 1
export LD_LIBRARY_PATH=$PWD/t/usr/lib/x86_64-linux-gnu
Z7=$PWD/t/usr/lib/7zip/7z; RAR=$PWD/t/usr/bin/rar; ZS=$PWD/t/usr/bin/zstd; ZP=$PWD/t/usr/bin/zpaq
S=${LAB:-.}; R=$S/race.csv; echo "set,tool,bytes,seconds" > $R
run() { local s=$1 n=$2 f=$3; shift 3; rm -rf out; mkdir out; local t0=$(date +%s.%N); bash -c "$*" >/dev/null 2>log.txt || echo "FAIL $s $n: $(tail -1 log.txt)" >> $S/race.err
  echo "$s,$n,$(stat -c %s $f 2>/dev/null || echo NA),$(echo "$(date +%s.%N) - $t0" | bc)" >> $R; }
for s in jpeg png text binary zipped mail; do
  rm -rf o x; cp -r $S/corpus/$s o; cp -r $S/tr/$s x
  orig=$(du -sb o --apparent-size | cut -f1); echo "$s,original,$orig,0" >> $R
  run $s 7z out/a.7z   "cd o && $Z7 a -t7z -mx=9 -md=512m -mfb=273 -ms=on -mmt=2 ../out/a.7z ."
  run $s rar out/a.rar "cd o && $RAR a -r -s -m5 -md1g -mt6 ../out/a.rar ."
  run $s zstd22 out/a.zst "tar -C o --sort=name -cf - . | $ZS --ultra -22 --long=31 -T1 -q -o out/a.zst"
  run $s zpaq5 out/a.zpaq "cd o && $ZP a ../out/a.zpaq . -m5 -t6"
  run $s ours+zstd3 out/a.zst "tar -C x -cf - . | $ZS -3 --long=31 -T6 -q -o out/a.zst"
  run $s ours+7z out/a.7z "cd x && $Z7 a -t7z -mx=9 -md=512m -mfb=273 -ms=on -mmt=2 ../out/a.7z ."
  run $s ours+zpaq5 out/a.zpaq "cd x && $ZP a ../out/a.zpaq . -m5 -t6"
done
echo finished >> $R
