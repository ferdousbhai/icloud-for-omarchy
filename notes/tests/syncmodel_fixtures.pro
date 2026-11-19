QT -= gui
QT += core
CONFIG += c++17 console
CONFIG -= app_bundle
TARGET = syncmodel_fixtures
TEMPLATE = app

SOURCES += syncmodel_fixtures.cpp
INCLUDEPATH += ../src
# The committed cases and what the C++ answered for them.
DEFINES += SYNCMODEL_FIXTURES=\\\"$$PWD/fixtures/syncmodel.json\\\"
