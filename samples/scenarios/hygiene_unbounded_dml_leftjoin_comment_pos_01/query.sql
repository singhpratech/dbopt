-- A comment between LEFT and JOIN must not turn an outer join into an inner
-- one. This still rewrites every row of dbo.T.
UPDATE t SET Flag = 1 FROM dbo.T t LEFT /* outer */ JOIN dbo.U u ON u.tid = t.Id;
