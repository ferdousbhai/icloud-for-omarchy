QT += core gui qml quick quickcontrols2 printsupport dbus
CONFIG += c++17 console
CONFIG -= app_bundle
TARGET = layout_test
TEMPLATE = app

SOURCES += layout_test.cpp \
    ../src/notesbackend.cpp \
    ../src/markdownhighlighter.cpp
HEADERS += ../src/notesbackend.h ../src/vaultlock.h ../src/markdownhighlighter.h ../src/notetree.h
INCLUDEPATH += ../src
# The window under test: the real one, read from the checkout.
DEFINES += NOTES_QML=\\\"$$PWD/../qml/main.qml\\\"
