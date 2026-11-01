# The driver evaluates the whole file as one top-level task, so the callbacks are registered,
# listed and removed here but never run.
counter <- addTaskCallback(function(expr, value, ok, visible) TRUE, name = "counter")
counter
unnamed <- addTaskCallback(function(expr, value, ok, visible, data) TRUE, data = 1)
unnamed
getTaskCallbackNames()
removeTaskCallback("counter")
removeTaskCallback("counter")
getTaskCallbackNames()
removeTaskCallback(1)
removeTaskCallback(7)
getTaskCallbackNames()
tryCatch(addTaskCallback(1), error = conditionMessage)
