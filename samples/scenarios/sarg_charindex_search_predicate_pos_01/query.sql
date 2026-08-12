CREATE TABLE dbo.Articles (ArticleId int NOT NULL, Body varchar(4000) NOT NULL);
GO
SELECT ArticleId FROM dbo.Articles WHERE CHARINDEX('needle', Body) > 0;
