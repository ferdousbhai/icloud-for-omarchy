// Console asserts shared by the tests: no framework in this repo by design.
// A non-zero exit means a regression.
#ifndef CHECK_H
#define CHECK_H

#include <QTextStream>
#include <cstdlib>

namespace {
int failures = 0;

void check(bool cond, const char *name)
{
    QTextStream(stdout) << (cond ? "ok " : "FAIL ") << name << "\n";
    if (!cond)
        ++failures;
}

int report()
{
    QTextStream(stdout) << (failures ? "RESULT FAIL\n" : "RESULT OK\n");
    return failures ? EXIT_FAILURE : EXIT_SUCCESS;
}
} // namespace

#endif
