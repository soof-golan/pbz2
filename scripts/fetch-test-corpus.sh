#!/bin/sh
set -eu
cd "$(dirname "$0")/.."

if [ -d testdata/bzip2-tests ]; then
  git -C testdata/bzip2-tests pull --ff-only
else
  git clone --depth 1 https://sourceware.org/git/bzip2-tests.git testdata/bzip2-tests
fi

mkdir -p testdata/more/commons-compress
curl -fsSL -o testdata/more/commons-compress/my10m.tar.bz2 \
  https://raw.githubusercontent.com/apache/commons-compress/master/src/test/resources/org/apache/commons/compress/COMPRESS-651/my10m.tar.bz2
echo "a86f7a8ac0adf964f62c2f1e7f636a24  -" > testdata/more/commons-compress/my10m.tar.md5
