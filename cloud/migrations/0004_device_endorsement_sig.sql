-- The endorsement's signature, so a client can check a later device's chain
-- back to the first one itself. Null for a first device, and for devices
-- registered before this column.
ALTER TABLE devices ADD COLUMN endorsement_sig TEXT;
