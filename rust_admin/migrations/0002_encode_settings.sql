ALTER TABLE videos ADD COLUMN encode_settings TEXT;
ALTER TABLE video_variants ADD COLUMN video_codec TEXT NOT NULL DEFAULT 'h264';
ALTER TABLE video_variants ADD COLUMN segment_container TEXT NOT NULL DEFAULT 'mpegts';
