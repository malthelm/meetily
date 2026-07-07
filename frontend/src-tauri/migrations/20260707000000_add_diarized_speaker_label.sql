-- Add diarized human speaker labels to transcript segments.
-- This is separate from the older `speaker` column, which was used for audio
-- source labels such as mic/system.

ALTER TABLE transcripts ADD COLUMN speaker_label TEXT;
