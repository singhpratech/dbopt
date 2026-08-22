-- hierarchyid / spatial methods are built-in CLR type methods, and a
-- table-valued function in CROSS APPLY is a row source, not a predicate.
SELECT e.[OrganizationNode].ToString() AS NodePath, g.Location.STAsText() AS Loc
FROM dbo.Employees AS e
CROSS APPLY tSQLt.Private_GetDropItemCmd(e.EmployeeId) AS dc
JOIN dbo.Sites AS g ON g.SiteId = e.SiteId
WHERE e.[OrganizationNode].IsDescendantOf(@root) = 1
  AND g.Location.STDistance(@here) < 1000;
