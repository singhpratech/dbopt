-- Cyrillic / CJK identifiers. The suggested index must fire and carry a
-- readable name (letters survive the sanitiser) instead of IX____________.
SELECT [клиент].[Идентификатор], [клиент].[Имя], [注文].[金額]
FROM dbo.[Клиенты] AS [клиент]
INNER JOIN dbo.[注文] AS [注文] ON [注文].[КлиентId] = [клиент].[Идентификатор]
WHERE [клиент].[Идентификатор] = 10
  AND [注文].[日付] >= '2025-01-01';
