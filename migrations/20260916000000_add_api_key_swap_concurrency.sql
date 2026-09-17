ALTER TABLE api_keys
ADD COLUMN swap_max_concurrent INTEGER
CHECK (swap_max_concurrent IS NULL OR swap_max_concurrent > 0);
