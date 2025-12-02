# ZERO-REGEX VERSION — works on BusyBox, mawk, gawk, POSIX awk

# Helper: trim leading spaces
function ltrim(s) {
    while (substr(s,1,1) == " " || substr(s,1,1) == "\t")
        s = substr(s,2)
    return s
}

{
    line = $0

    # ---------- Detect comment type ----------
    if (substr(line,1,3) == "///")
        type = "doc"
    else if (substr(line,1,3) == "//!")
        type = "innerdoc"
    else if (substr(line,1,2) == "//")
        type = "line"
    else
        type = "none"

    # ---------- Handle comment lines ----------
    if (type != "none") {

        # Determine prefix
        if (type == "doc")
            prefix = "///"
        else if (type == "innerdoc")
            prefix = "//!"
        else
            prefix = "//"

        # Extract text after prefix
        raw = substr(line, length(prefix) + 1)
        text = raw
        if (substr(text,1,1) == " ")
            text = substr(text,2)

        # ---------- 1. Empty comment line: only spaces ----------
        empty = 1
        for (i=1; i<=length(text); i++) {
            c = substr(text,i,1)
            if (c != " " && c != "\t") {
                empty = 0
                break
            }
        }
        if (empty) {
            if (in_comment) {
                print out_prefix merged
                in_comment = 0
            }
            print line
            next
        }

        # ---------- 2. "===..." test ----------
        trimmed = ltrim(raw)
        if (substr(trimmed,1,3) == "===") {
            if (in_comment) {
                print out_prefix merged
                in_comment = 0
            }
            print line
            next
        }

        # ---------- 3. Mergeable comment lines ----------
        if (in_comment && type == last_type) {
            merged = merged " " text
        } else {
            if (in_comment)
                print out_prefix merged

            in_comment = 1
            last_type = type
            merged = text
            out_prefix = prefix " "
        }

        next
    }

    # ---------- Non-comment line ----------
    if (in_comment) {
        print out_prefix merged
        in_comment = 0
    }
    print line
}

END {
    if (in_comment)
        print out_prefix merged
}

