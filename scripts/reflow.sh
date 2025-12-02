find . -name target -prune -name generated -prune -o -type f -name "*.rs" -print0 |
  while read -r -d $'\0' x; do
    awk -i inplace -f scripts/reflow.awk "$x"
  done
