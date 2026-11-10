QT -= gui
QT += core
CONFIG += c++17 console
CONFIG -= app_bundle
TARGET = guardrails_test
TEMPLATE = app

SOURCES += guardrails_test.cpp
INCLUDEPATH += ../src
# The window's QML, which a guardrail reads.
DEFINES += NOTES_QML=\\\"$$PWD/../qml/main.qml\\\"
