-- Add signature column to block_headers table
ALTER TABLE block_headers ADD COLUMN signature BLOB NOT NULL DEFAULT '';

-- Update existing rows to have empty signature (will be populated later if needed)
-- The default empty blob will be used for existing entries
