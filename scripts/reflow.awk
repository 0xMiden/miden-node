# ZERO-REGEX VERSION — supports list-like comment lines:
#   * bullet points
#   - dashes
#   1. numbered lists
#   1) numbered lists
#
# These lines break merge groups and are printed verbatim.

function ltrim(s) {
    while (substr(s,1,1) == " " || substr(s,1,1) == "\t")
        s = substr(s,2)
    return s
}

function is_numbered_list(s,    i, c) {
    # returns 1 if s begins with digits then '.' or ')'
    found_digit = 0
    for (i=1; i<=length(s); i++) {
        c = substr(s,i,1)
        if (c >= "0" && c <= "9") {
            found_digit = 1
            continue
        }
        # if first non-digit is . or ) and we saw a digit
        if (found_digit && (c == "." || c == ")"))
            return 1
        return 0
    }
    return 0
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

    # ---------- COMMENT LINES ----------
    if (type != "none") {

        # Determine prefix
        if (type == "doc")
            prefix = "///"
        else if (type == "innerdoc")
            prefix = "//!"
        else
            prefix = "//"

        # Extract content after prefix
        raw = substr(line, length(prefix) + 1)
        text = raw
        if (substr(text,1,1) == " ")
            text = substr(text,2)

        # Trimmed version for tests
        trimmed = ltrim(raw)

        # ---------- 1. Empty comment line ----------
        empty = 1
        for (i=1; i<=length(text); i++) {
            c = substr(text,i,1)
            if (c != " " && c != "\t") { empty = 0; break }
        }
        if (empty) {
            if (in_comment) print out_prefix merged
            in_comment = 0
            print line
            next
        }

        # ---------- 2. "===..." lines ----------
        if (substr(trimmed,1,3) == "===") {
            if (in_comment) print out_prefix merged
            in_comment = 0
            print line
            next
        }

        # ---------- 3. Bullet point or list item ----------
        first = substr(trimmed,1,1)
        if (first == "*" || first == "-") {
            if (in_comment) print out_prefix merged
            in_comment = 0
            print line
            next
        }

        # Numbered list detection
        if (is_numbered_list(trimmed)) {
            if (in_comment) print out_prefix merged
            in_comment = 0
            print line
            next
        }

        # ---------- 4. Mergeable line ----------
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

