-- datetime2 has better range and precision at equal or smaller storage.
CREATE TABLE dbo.Events (EventId int NOT NULL, OccurredAt datetime2(3) NOT NULL);
