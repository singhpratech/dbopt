-- A variable needle searched in a column is the classic substring test.
SELECT ArticleId FROM dbo.Articles WHERE CHARINDEX(@needle, Body) > 0;
