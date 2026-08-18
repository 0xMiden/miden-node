-- Inserts a block header, replacing the existing header if one is already stored at this height.
REPLACE INTO block_headers (block_num, block_header)
VALUES (?1, ?2);
