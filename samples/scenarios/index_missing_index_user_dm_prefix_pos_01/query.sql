-- A user table whose name merely starts with `dm_` is not a DMV. Skipping it by
-- name prefix silently withheld real advice.
SELECT Id, Payload FROM dbo.dm_snapshots WHERE CapturedAt > @since;
