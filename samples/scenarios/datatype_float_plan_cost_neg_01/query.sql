-- Optimizer plan costs are unitless floats straight out of showplan XML.
CREATE TABLE #plan_costs (
    QueryPlanCost float NULL,
    key_lookup_cost float NULL,
    sort_cost float NULL,
    index_spool_cost float NULL,
    StatementSubTreeCost float NULL
);
