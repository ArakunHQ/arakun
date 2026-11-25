-- A cloud session is bound to one machine: the label it may claim at
-- /machines/<label>/pc and nothing else. Every other kind has none. The check
-- keeps a cloud session without a label, which would be good for no door, or a
-- label on another kind, which would narrow nothing, from ever being stored.
ALTER TABLE sessions ADD COLUMN machine_label TEXT
  CHECK ((kind = 'cloud') = (machine_label IS NOT NULL));
