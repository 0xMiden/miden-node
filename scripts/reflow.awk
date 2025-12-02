# Merge Rust comment lines while preserving:
# - original spacing after //, ///, //! 
# - empty lines
# - === headings
# - bullets / numbered lists
# - fenced code blocks
# - Markdown headings

function ltrim(s) {
    while (substr(s,1,1)==" " || substr(s,1,1)=="\t") s=substr(s,2)
    return s
}

function is_numbered_list(s,    i,c) {
    found_digit=0
    for(i=1;i<=length(s);i++){
        c=substr(s,i,1)
        if(c>="0" && c<="9"){found_digit=1; continue}
        if(found_digit && (c=="."||c==")")) return 1
        return 0
    }
    return 0
}

{
    line=$0

    # Detect comment type
    if(substr(line,1,3)=="///") type="doc"
    else if(substr(line,1,3)=="//!") type="innerdoc"
    else if(substr(line,1,2)=="//") type="line"
    else type="none"

    if(type!="none"){
        if(type=="doc") prefix="///"
        else if(type=="innerdoc") prefix="//!"
        else prefix="//"

        raw=substr(line,length(prefix)+1)
        # Capture the original spaces after prefix
        match_space=""
        i=1
        while(i<=length(raw) && (substr(raw,i,1)==" " || substr(raw,i,1)=="\t")){ match_space = match_space substr(raw,i,1); i++ }
        text=substr(raw,i)

        trimmed=ltrim(raw)

        # ---------- Fenced code block ----------
        if(substr(trimmed,1,3)=="```"){
            if(in_comment){ print out_prefix merged; in_comment=0; out_space="" }
            if(fenced==0) fenced=1; else fenced=0
            print line
            next
        }
        if(fenced==1){ print line; next }

        # ---------- Empty line ----------
        empty=1
        for(j=1;j<=length(trimmed);j++){c=substr(trimmed,j,1); if(c!=" " && c!="\t"){empty=0; break}}
        if(empty){ if(in_comment){print out_prefix merged; in_comment=0; out_space=""} print line; next }

        # ---------- === heading ----------
        if(substr(trimmed,1,3)=="==="){ if(in_comment){print out_prefix merged; in_comment=0; out_space=""} print line; next }

        # ---------- Markdown heading ----------
        if(substr(trimmed,1,1)=="#"){ if(in_comment){print out_prefix merged; in_comment=0; out_space=""} print line; next }

        # ---------- Bullet or numbered list ----------
        first=substr(trimmed,1,1)
        if(first=="*" || first=="-" || is_numbered_list(trimmed)){ if(in_comment){print out_prefix merged; in_comment=0; out_space=""} print line; next }

        # ---------- Mergeable line ----------
        if(in_comment && type==last_type){
            merged=merged " " text
        } else {
            if(in_comment) print out_prefix merged
            in_comment=1
            last_type=type
            merged=text
            out_prefix = prefix match_space
        }

        next
    }

    # Non-comment line
    if(in_comment){ print out_prefix merged; in_comment=0; out_space="" }
    print line
}

END { if(in_comment) print out_prefix merged }

