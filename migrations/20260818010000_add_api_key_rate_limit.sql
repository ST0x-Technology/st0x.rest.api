ALTER TABLE api_keys
ADD COLUMN rate_limit_rpm INTEGER
CHECK (rate_limit_rpm IS NULL OR rate_limit_rpm > 0);
