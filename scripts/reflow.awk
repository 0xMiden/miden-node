# ZERO-REGEX VERSION — preserves indentation for merged comment lines

function ltrim(s) {
    while (substr(s,1,1) == " " || substr(s,1,1) == "\t")
        s = substr(s,2)
    return s
}

function leading_spaces(s,    i, count, c) {
    count=0
    for (i=1; i<=length(s); i++) {
        c=substr(s,i,1)
        if (c==" " || c=="\t") count++
        else break
    }
    return count
}

function is_numbered_list(s,    i, c) {
    found_digit=0
    for (i=1; i<=length(s); i++) {
        c=substr(s,i,1)
        if (c >= "0" && c <= "9") { found_digit=1; continue }
        if (found_digit && (c=="." || c==")")) return 1
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

    # ---------- Handle comment lines ----------
    if (type != "none") {

        prefix = (type=="doc" ? "///" : type=="innerdoc" ? "//!" : "//")
        raw = substr(line, length(prefix)+1)
        trimmed = ltrim(raw)

        # Track indentation
        indent = leading_spaces(raw)

        # ---------- Fenced code block ----------
        if (substr(trimmed,1,3) == "```") {
            if (in_comment) { print out_prefix merged; in_comment=0 }
            if (fenced==0) fenced=1; else fenced=0
            print line
            next
        }
        if (fenced==1) { print line; next }

        # ---------- Empty comment line ----------
        empty=1
        for (i=1; i<=length(trimmed); i++) {
            c=substr(trimmed,i,1)
            if (c!=" " && c!="\t") { empty=0; break }
        }
        if (empty) { if (in_comment) { print out_prefix merged; in_comment=0 } print line; next }

        # ---------- === section line ----------
        if (substr(trimmed,1,3) == "===") { if (in_comment) { print out_prefix merged; in_comment=0 } print line; next }

        # ---------- Markdown heading ----------
        if (substr(trimmed,1,1) == "#") { if (in_comment) { print out_prefix merged; in_comment=0 } print line; next }

        # ---------- Bullet or numbered list ----------
        first = substr(trimmed,1,1)
        if (first=="*" || first=="-" || is_numbered_list(trimmed)) { if (in_comment) { print out_prefix merged; in_comment=0 } print line; next }

        # ---------- Mergeable line ----------
        if (in_comment && type==last_type) {
            merged = merged " " ltrim(raw)  # preserve relative spacing inside line
            if (indent < out_indent) out_indent = indent
        } else {
            if (in_comment) print out_prefix merged
            in_comment=1
            last_type=type
            merged = ltrim(raw)
            out_prefix = prefix
            out_indent = indent
        }

        next
    }

    # ---------- Non-comment line ----------
    if (in_comment) {
        # Apply preserved indentation
        printf "%s%*s%s\n", out_prefix, out_indent, "", merged
        in_comment=0
    }
    print line
}

END {
    if (in_comment) printf "%s%*s%s\n", out_prefix, out_indent, "", merged
}

