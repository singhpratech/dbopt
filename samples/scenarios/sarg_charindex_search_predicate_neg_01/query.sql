-- CHARINDEX in the projection computes a value; it is not a search predicate,
-- so it costs nothing in seekability.
CREATE TABLE dbo.Articles (ArticleId int NOT NULL, Body varchar(4000) NOT NULL);
GO
SELECT ArticleId, CHARINDEX('needle', Body) AS Position FROM dbo.Articles WHERE ArticleId = 7;
