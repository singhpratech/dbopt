-- False-positive guard: statistics are refreshed with sampling plus
-- PERSIST_SAMPLE_PERCENT rather than the expensive FULLSCAN over every page.
-- There is no WITH FULLSCAN, so stats.update_statistics_fullscan_on_huge_table
-- must stay silent.
UPDATE STATISTICS dbo.Sales (IX_Sales_PlacedAt)
    WITH SAMPLE 10 PERCENT, PERSIST_SAMPLE_PERCENT = ON;
