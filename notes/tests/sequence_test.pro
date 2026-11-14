QT += core gui qml quick quickcontrols2 printsupport dbus
CONFIG += c++17 console
CONFIG -= app_bundle
TARGET = sequence_test
TEMPLATE = app

SOURCES += sequence_test.cpp \
    ../src/notesbackend.cpp \
    ../src/markdownhighlighter.cpp
HEADERS += ../src/notesbackend.h ../src/vaultlock.h ../src/markdownhighlighter.h ../src/syncmodel.h
INCLUDEPATH += ../src
# The window under test: the real one, read from the checkout.
DEFINES += NOTES_QML=\\\"$$PWD/../qml/main.qml\\\"
