-- ORDER BY 1 on a single-expression select list: there is nothing to reorder.
SELECT STUFF((SELECT ', ' + ag.name
              FROM sys.availability_groups AS ag
              ORDER BY 1
              FOR XML PATH('')), 1, 2, '') AS Details;
