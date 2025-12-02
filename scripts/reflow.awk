# ZERO-REGEX VERSION — preserves:
# - //, ///, //! comment lines
# - empty lines
# - === headings
# - bullet points (*, -, numbered lists)
# - fenced code blocks (lines starting with ``` inside comments)
# Merges only mergeable lines of the same comment type

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
        # first non-digit is . or ), and we saw a digit
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

    # ---------- Handle fenced code block start/end ----------
    if (type != "none") {
        prefix = (type=="doc" ? "///" : type=="innerdoc" ? "//!" : "//")
        raw = substr(line, length(prefix)+1)
        trimmed = ltrim(raw)

        # Detect fenced code block line
        if (substr(trimmed,1,3) == "```") {
            # Flush any existing merged comment
            if (in_comment) {
                print out_prefix merged
                in_comment = 0
            }

            # Toggle fenced code block mode
            if (fenced == 0) fenced = 1
            else fenced = 0

            # Print the code block fence line verbatim
            print line
            next
        }

        # If inside a fenced code block, print everything verbatim
        if (fenced == 1) {
            print line
            next
        }

        # ---------- Empty comment line ----------
        empty = 1
        text = raw
        if (substr(text,1,1) == " ")
            text = substr(text,2)
        for (i=1; i<=length(text); i++) {
            c = substr(text,i,1)
            if (c != " " && c != "\t") { empty = 0; break }
        }
        if (empty) {
            if (in_comment) { print out_prefix merged; in_comment=0 }
            print line
            next
        }

        # ---------- === heading ----------
        if (substr(trimmed,1,3) == "===") {
            if (in_comment) { print out_prefix merged; in_comment=0 }
            print line
            next
        }

        # ---------- Bullet or list item ----------
        first = substr(trimmed,1,1)
        if (first=="*" || first=="-" || is_numbered_list(trimmed)) {
            if (in_comment) { print out_prefix merged; in_comment=0 }
            print line
            next
        }

        # ---------- Mergeable line ----------
        if (in_comment && type == last_type) {
            merged = merged " " text
        } else {
            if (in_comment) print out_prefix merged
            in_comment = 1
            last_type = type
            merged = text
            out_prefix = prefix " "
        }

        next
    }

    # ---------- Non-comment line ----------
    if (in_comment) { print out_prefix merged; in_comment=0 }
    print line
}

END {
    if (in_comment) print out_prefix merged
}

