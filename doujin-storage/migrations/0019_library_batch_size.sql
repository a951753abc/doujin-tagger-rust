ALTER TABLE application_settings
ADD COLUMN library_batch_size INTEGER NOT NULL DEFAULT 48
CHECK (library_batch_size IN (24, 48, 96, 144, 192));
