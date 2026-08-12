-- CASCADE and TOP are keywords in these positions, not unqualified tables.
ALTER TABLE dbo.A ADD CONSTRAINT FK FOREIGN KEY (x) REFERENCES dbo.B (y) ON UPDATE CASCADE;
UPDATE TOP (10) dbo.T SET x = 1 WHERE Id = 1;
