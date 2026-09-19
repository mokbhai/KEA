-- Where a meeting's title came from: 'calendar' | 'llm' | 'user'.
--
-- NULL on every existing row and read as 'llm', which is what those rows in
-- fact are. The column exists so a user rename survives a re-synthesis, and so
-- the "from Calendar" chip has something to branch on.
ALTER TABLE meetings ADD COLUMN title_source TEXT;
