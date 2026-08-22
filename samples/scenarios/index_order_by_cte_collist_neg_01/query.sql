-- The proposed index would target a recursive CTE declared with the column-list
-- form `WITH name (cols) AS (...)`. A CTE cannot be indexed.
WITH [BOM_cte] ([ProductAssemblyID], [ComponentID], [BOMLevel]) AS (
    SELECT b.[ProductAssemblyID], b.[ComponentID], 0
    FROM   [Production].[BillOfMaterials] AS b
    WHERE  b.[ProductAssemblyID] = @StartProductID
    UNION ALL
    SELECT b.[ProductAssemblyID], b.[ComponentID], cte.[BOMLevel] + 1
    FROM   [BOM_cte] AS cte
    INNER JOIN [Production].[BillOfMaterials] AS b
        ON b.[ProductAssemblyID] = cte.[ComponentID]
)
SELECT b.[ProductAssemblyID], b.[ComponentID], b.[BOMLevel]
FROM   [BOM_cte] AS b
WHERE  b.[BOMLevel] = 2
ORDER BY b.[BOMLevel], b.[ProductAssemblyID], b.[ComponentID]
OPTION (MAXRECURSION 25);
